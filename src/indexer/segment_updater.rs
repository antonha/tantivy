#![allow(for_kv_map)]

use core::Index;
use core::IndexMeta;
use core::META_FILEPATH;
use core::Segment;
use core::SegmentId;
use core::SegmentMeta;
use core::SerializableSegment;
use directory::Directory;
use Error;
use futures_cpupool::CpuPool;
use futures::{Future, future};
use futures::Canceled;
use futures::oneshot;
use indexer::{MergePolicy, DefaultMergePolicy};
use indexer::delete_queue::DeleteQueue;
use indexer::index_writer::advance_deletes;
use indexer::MergeCandidate;
use indexer::merger::IndexMerger;
use indexer::SegmentEntry;
use indexer::SegmentSerializer;
use Result;
use rustc_serialize::json;
use schema::Schema;
use std::borrow::BorrowMut;
use std::collections::HashMap;
use std::io::Write;
use std::mem;
use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::RwLock;
use std::thread;
use std::thread::JoinHandle;
use super::segment_manager::{SegmentManager, get_segments};


/// Save the index meta file.
/// This operation is atomic :
/// Either
//  - it fails, in which case an error is returned,
/// and the `meta.json` remains untouched,
/// - it success, and `meta.json` is written
/// and flushed.
///
/// This method is not part of tantivy's public API
pub fn save_new_metas(schema: Schema,
                  opstamp: u64,
                  directory: &mut Directory)
                  -> Result<()> {
    save_metas(vec!(), schema, opstamp, directory)
}



/// Save the index meta file.
/// This operation is atomic :
/// Either
//  - it fails, in which case an error is returned,
/// and the `meta.json` remains untouched,
/// - it success, and `meta.json` is written
/// and flushed.
///
/// This method is not part of tantivy's public API
pub fn save_metas(segment_metas: Vec<SegmentMeta>,
                  schema: Schema,
                  opstamp: u64,
                  directory: &mut Directory)
                  -> Result<()> {
    let metas = IndexMeta {
        segments: segment_metas,
        schema: schema,
        opstamp: opstamp,
    };
    let mut w = Vec::new();
    try!(write!(&mut w, "{}\n", json::as_pretty_json(&metas)));
    Ok(directory
        .atomic_write(&META_FILEPATH, &w[..])?)
        
}



// The segment update runner is in charge of processing all
//  of the `SegmentUpdate`s.
//
// All this processing happens on a single thread
// consuming a common queue. 
#[derive(Clone)]
pub struct SegmentUpdater(Arc<InnerSegmentUpdater>);


struct InnerSegmentUpdater {
    pool: CpuPool,
    index: Index,
    segment_manager: SegmentManager,
    merge_policy: RwLock<Box<MergePolicy>>,
    merging_thread_id: AtomicUsize,
    merging_threads: RwLock<HashMap<usize, JoinHandle<Result<SegmentEntry>>>>,
    generation: AtomicUsize,
    delete_queue: DeleteQueue,
}

impl SegmentUpdater {

    pub fn new(index: Index, delete_queue: DeleteQueue) -> Result<SegmentUpdater> {
        let segments = index.segments()?;
        let segment_manager = SegmentManager::from_segments(segments);
        Ok(
            SegmentUpdater(Arc::new(InnerSegmentUpdater {
                pool: CpuPool::new(1),
                index: index,
                segment_manager: segment_manager,
                merge_policy: RwLock::new(box DefaultMergePolicy::default()),
                merging_thread_id: AtomicUsize::default(),
                merging_threads: RwLock::new(HashMap::new()),
                generation: AtomicUsize::default(),
                delete_queue: delete_queue,
            }))
        )
    }

    pub fn get_merge_policy(&self) -> Box<MergePolicy> {
        self.0.merge_policy.read().unwrap().box_clone()
    }

    pub fn set_merge_policy(&self, merge_policy: Box<MergePolicy>) {
        *self.0.merge_policy.write().unwrap()= merge_policy;
    }

    fn get_merging_thread_id(&self) -> usize {
        self.0.merging_thread_id.fetch_add(1, Ordering::SeqCst)
    }


    fn run_async<T: 'static + Send, F: 'static + Send + FnOnce(SegmentUpdater) -> T>(&self, f: F) -> impl Future<Item=T, Error=Error> {
        let me_clone = self.clone();
        self.0.pool.spawn_fn(move || {
            Ok(f(me_clone))
        })
    }

    pub fn rollback(&mut self, generation: usize) -> impl Future<Item=(), Error=Error> {
        self.0.generation.store(generation, Ordering::Release);
        self.run_async(|segment_updater| {
            segment_updater.0.segment_manager.rollback();
        })
    }

    pub fn add_segment(&self, generation: usize, segment_entry: SegmentEntry) -> impl Future<Item=bool, Error=Error> {
        if generation >= self.0.generation.load(Ordering::Acquire) {
            future::Either::A(self.run_async(|segment_updater| {
                segment_updater.0.segment_manager.add_segment(segment_entry);
                segment_updater.consider_merge_options();
                true
            }))
        }
        else {
            future::Either::B(future::ok(false))
        }
    }

    fn purge_deletes(&self) -> Result<Vec<SegmentMeta>> {
        self.0.segment_manager
            .segment_entries()
            .into_iter()
            .map(|segment_entry| {
                let mut segment = self.0.index.segment(segment_entry.meta().clone()); 
                advance_deletes(&mut segment, &self.0.delete_queue.snapshot(), segment_entry.doc_to_opstamp())
            })
            .collect()
    }

    pub fn commit(&self, opstamp: u64) -> impl Future<Item=(), Error=Error> {
        self.run_async(move |segment_updater| {
            let segment_metas = segment_updater.purge_deletes().expect("Failed purge deletes");
            segment_updater.0.segment_manager.commit(segment_metas);
            let mut directory = segment_updater.0.index.directory().box_clone();
            save_metas(
                    segment_updater.0.segment_manager.committed_segment_metas(),
                    segment_updater.0.index.schema(),
                    opstamp,
                    directory.borrow_mut()).expect("Could not save metas.");
            segment_updater.consider_merge_options();
        })
    }


    pub fn start_merge(&self, segment_ids: &[SegmentId]) -> impl Future<Item=SegmentEntry, Error=Canceled> {
        
        self.0.segment_manager.start_merge(segment_ids);
        let segment_updater_clone = self.clone();
        
        let segment_ids_vec = segment_ids.to_vec(); 
        
        let merging_thread_id = self.get_merging_thread_id();
        let (merging_future_send, merging_future_recv) = oneshot();
        
        let delete_operations = self.0.delete_queue.snapshot();

        if segment_ids.is_empty() {
            return merging_future_recv;
        }
        
        let merging_join_handle = thread::spawn(move || {
            
            // first we need to apply deletes to our segment.
            info!("Start merge: {:?}", segment_ids_vec);
            
            let ref index = segment_updater_clone.0.index;
            let schema = index.schema();

            let mut segment_metas = vec!();
            for segment_id in &segment_ids_vec {
                if let Some(segment_entry) = segment_updater_clone.0
                    .segment_manager
                    .segment_entry(segment_id) {
                    let mut segment = index.segment(segment_entry.meta().clone());
                    let segment_meta = advance_deletes(
                         &mut segment,
                         &delete_operations,
                         segment_entry.doc_to_opstamp())?;
                    segment_metas.push(segment_meta);
                }
                else {
                    error!("Error, had to abort merge as some of the segment is not managed anymore.a");
                    return Err(Error::InvalidArgument(format!("Segment {:?} requested for merge is not managed.", segment_id)));
                }
            }
            
            let segments: Vec<Segment> = segment_metas
                .iter()
                .cloned()
                .map(|segment_meta| index.segment(segment_meta))
                .collect();
            
            // An IndexMerger is like a "view" of our merged segments.
            let merger: IndexMerger = IndexMerger::open(schema, &segments[..])?;
            let mut merged_segment = index.new_segment(); 
            
            // ... we just serialize this index merger in our new segment
            // to merge the two segments.

            let segment_serializer = SegmentSerializer::for_segment(&mut merged_segment).expect("Creating index serializer failed");

            let num_docs = merger.write(segment_serializer).expect("Serializing merged index failed");
            let mut segment_meta = SegmentMeta::new(merged_segment.id());
            segment_meta.set_max_doc(num_docs);
            
            let segment_entry = SegmentEntry::new(segment_meta);
            segment_updater_clone
                .end_merge(segment_metas.clone(), segment_entry.clone())
                .wait()
                .unwrap();
            merging_future_send.complete(segment_entry.clone());
            segment_updater_clone.0.merging_threads.write().unwrap().remove(&merging_thread_id);
            Ok(segment_entry)
        });
        self.0.merging_threads.write().unwrap().insert(merging_thread_id, merging_join_handle);
        merging_future_recv
    }


    fn consider_merge_options(&self) {
        let (committed_segments, uncommitted_segments) = get_segments(&self.0.segment_manager);
        // Committed segments cannot be merged with uncommitted_segments.
        // We therefore consider merges using these two sets of segments independently.
        let merge_policy = self.get_merge_policy();
        let mut merge_candidates = merge_policy.compute_merge_candidates(&uncommitted_segments);
        let committed_merge_candidates = merge_policy.compute_merge_candidates(&committed_segments);
        merge_candidates.extend_from_slice(&committed_merge_candidates[..]);
        for MergeCandidate(segment_metas) in merge_candidates {
            self.start_merge(&segment_metas);
        }
    }

    
    fn end_merge(&self, 
        merged_segment_metas: Vec<SegmentMeta>,
        resulting_segment_entry: SegmentEntry) -> impl Future<Item=(), Error=Error> {
        
        self.run_async(move |segment_updater| {
            segment_updater.0.segment_manager.end_merge(&merged_segment_metas, resulting_segment_entry);
            let mut directory = segment_updater.0.index.directory().box_clone();
            let segment_metas = segment_updater.0.segment_manager.committed_segment_metas();
            save_metas(
                segment_metas,
                segment_updater.0.index.schema(),
                segment_updater.0.index.opstamp(),
                directory.borrow_mut()).expect("Could not save metas.");
            for segment_meta in merged_segment_metas {
                segment_updater.0.index.delete_segment(segment_meta.id());
            }
        })
        
    }

    pub fn wait_merging_thread(&self) -> thread::Result<()> {
        let mut new_merging_threads = HashMap::new();
        {
            let mut merging_threads = self.0.merging_threads.write().unwrap();
            mem::swap(&mut new_merging_threads, merging_threads.deref_mut());
        }
        for (_, merging_thread_handle) in new_merging_threads {
            merging_thread_handle
                .join()
                .map(|_| ())?
        }
        Ok(())
    }

}




#[cfg(test)]
mod tests {

    use Index;
    use schema::*;
    use indexer::merge_policy::tests::MergeWheneverPossible;

    #[test]
    fn test_delete_during_merge() {
        let mut schema_builder = SchemaBuilder::default();
        let text_field = schema_builder.add_text_field("text", TEXT);
        let schema = schema_builder.build();

        let index = Index::create_in_ram(schema);
        
        // writing the segment
        let mut index_writer = index.writer_with_num_threads(1, 40_000_000).unwrap();
        index_writer.set_merge_policy(box MergeWheneverPossible);

        {
            for i in 0..100 {
                index_writer.add_document(doc!(text_field=>"a"));
                index_writer.add_document(doc!(text_field=>"b"));
            }
            assert!(index_writer.commit().is_ok());
        }
        
        {
            for i in 0..100 {
                index_writer.add_document(doc!(text_field=>"c"));
                index_writer.add_document(doc!(text_field=>"d"));
            }
            assert!(index_writer.commit().is_ok());
        }
        
        {
            index_writer.add_document(doc!(text_field=>"e"));
            index_writer.add_document(doc!(text_field=>"f"));
            assert!(index_writer.commit().is_ok());
        }

        {
            let term = Term::from_field_text(text_field, "a");
            index_writer.delete_term(term);
            assert!(index_writer.commit().is_ok());
        }

        index.load_searchers();
        assert_eq!(index.searcher().segment_readers().len(), 3);
        assert_eq!(index.searcher().num_docs(), 302);

        {
            index_writer.wait_merging_threads()
                        .expect("waiting for merging threads");
        }

        index.load_searchers();
        assert_eq!(index.searcher().segment_readers().len(), 2);
        assert_eq!(index.searcher().num_docs(), 302);
    }
}