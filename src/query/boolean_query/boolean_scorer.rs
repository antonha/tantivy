use query::Scorer;
use DocId;
use Score;
use std::collections::BinaryHeap;
use std::cmp::Ordering;
use postings::DocSet;
use query::OccurFilter;
use query::boolean_query::ScoreCombiner;


/// Each `HeapItem` represents the head of
/// a segment postings being merged.
///
/// * `doc` - is the current doc id for the given segment postings 
/// * `ord` - is the ordinal used to identify to which segment postings
/// this heap item belong to.
#[derive(Eq, PartialEq)]
struct HeapItem {
    doc: DocId,
    ord: u32,
}

/// `HeapItem` are ordered by the document
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other:&Self) -> Ordering {
         (other.doc).cmp(&self.doc)
    }
}

pub struct BooleanScorer<TScorer: Scorer> {
    postings: Vec<TScorer>,
    queue: BinaryHeap<HeapItem>,
    doc: DocId,
    score_combiner: ScoreCombiner,
    filter: OccurFilter,
}

impl<TScorer: Scorer> BooleanScorer<TScorer> {
    
    pub fn set_score_combiner(&mut self, score_combiner: ScoreCombiner)  {
        self.score_combiner = score_combiner;
    }
    
    pub fn new(postings: Vec<TScorer>,
               filter: OccurFilter) -> BooleanScorer<TScorer> {
        let score_combiner = ScoreCombiner::default_for_num_scorers(postings.len());
        let mut non_empty_postings: Vec<TScorer> = Vec::new();
        for mut posting in postings {
            let non_empty = posting.advance();
            if non_empty {
                non_empty_postings.push(posting);
            }
        }
        let heap_items: Vec<HeapItem> = non_empty_postings
            .iter()
            .map(|posting| posting.doc())
            .enumerate()
            .map(|(ord, doc)| {
                HeapItem {
                    doc: doc,
                    ord: ord as u32
                }
            })
            .collect();
        BooleanScorer {
            postings: non_empty_postings,
            queue: BinaryHeap::from(heap_items),
            doc: 0u32,
            score_combiner: score_combiner,
            filter: filter,
            
        }
    }
    
    
    /// Advances the head of our heap (the segment postings with the lowest doc)
    /// It will also update the new current `DocId` as well as the term frequency
    /// associated with the segment postings.
    /// 
    /// After advancing the `SegmentPosting`, the postings is removed from the heap
    /// if it has been entirely consumed, or pushed back into the heap.
    /// 
    /// # Panics
    /// This method will panic if the head `SegmentPostings` is not empty.
    fn advance_head(&mut self,) {
        {
            let mut mutable_head = self.queue.peek_mut().unwrap();
            let cur_postings = &mut self.postings[mutable_head.ord as usize];
            if cur_postings.advance() {
                mutable_head.doc = cur_postings.doc();
                return;
            }
        }
        self.queue.pop();
    }
}

impl<TScorer: Scorer> DocSet for BooleanScorer<TScorer> {
    fn advance(&mut self,) -> bool {
        loop {
            self.score_combiner.clear();
            let mut ord_bitset = 0u64;
            match self.queue.peek() {
                Some(heap_item) => {
                    let ord = heap_item.ord as usize;
                    self.doc = heap_item.doc;
                    let score = self.postings[ord].score();
                    self.score_combiner.update(score);
                    ord_bitset |= 1 << ord;  
                }
                None => {
                    return false;
                }
            }
            self.advance_head();
            while let Some(&HeapItem {doc, ord}) = self.queue.peek() {
                if doc == self.doc {
                    let ord = ord as usize;
                    let score = self.postings[ord].score();
                    self.score_combiner.update(score);
                    ord_bitset |= 1 << ord;
                }
                else  {
                    break;
                }
                self.advance_head();
            } 
            if self.filter.accept(ord_bitset) {
                return true;
            }
        }
    }   
            
    fn doc(&self,) -> DocId {
        self.doc
    }
}

impl<TScorer: Scorer> Scorer for BooleanScorer<TScorer> {
    
    fn score(&self,) -> f32 {
        self.score_combiner.score()
    }
}




#[cfg(test)]
mod tests {
    
    use super::*;
    use postings::{DocSet, VecPostings};
    use query::Scorer;
    use query::OccurFilter;
    use query::term_query::TermScorer;
    use directory::Directory;
    use directory::RAMDirectory;
    use schema::Field;
    use super::super::ScoreCombiner;
    use std::path::Path;
    use query::Occur;
    use postings::SegmentPostingsTestFactory;
    use postings::Postings;
    use fastfield::{U32FastFieldReader, U32FastFieldWriter, FastFieldSerializer};

    
   
    fn abs_diff(left: f32, right: f32) -> f32 {
        (right - left).abs()
    }   
    
    lazy_static! {
        static ref segment_postings_test_factory: SegmentPostingsTestFactory = SegmentPostingsTestFactory::default();
    }
    
    #[test]
    pub fn test_boolean_scorer() {
        let occurs = vec!(Occur::Should, Occur::Should);
        let occur_filter = OccurFilter::new(&occurs);
       
        let left_fieldnorms = U32FastFieldReader::from(vec!(100,200,300));
        let left = segment_postings_test_factory.from_data(vec!(1, 2, 3));
        let left_scorer = TermScorer {
            idf: 1f32,
            fieldnorm_reader: left_fieldnorms,
            segment_postings: left,
        };
        
        let right_fieldnorms = U32FastFieldReader::from(vec!(15,25,35));
        let right = segment_postings_test_factory.from_data(vec!(1, 3, 8));
        let mut right_scorer = TermScorer {
            idf: 4f32,
            fieldnorm_reader: right_fieldnorms,
            segment_postings: right,
        };
        let score_combiner = ScoreCombiner::from(vec!(0f32, 1f32, 2f32));
        let mut boolean_scorer = BooleanScorer::new(vec!(left_scorer, right_scorer), occur_filter);
        boolean_scorer.set_score_combiner(score_combiner);
        assert_eq!(boolean_scorer.next(), Some(1u32));
        assert!(abs_diff(boolean_scorer.score(), 1.7414213) < 0.001);
        assert_eq!(boolean_scorer.next(), Some(2u32));
        assert!(abs_diff(boolean_scorer.score(), 0.057735026) < 0.001f32);
        assert_eq!(boolean_scorer.next(), Some(3u32));
        assert_eq!(boolean_scorer.next(), Some(8u32));
        assert!(abs_diff(boolean_scorer.score(), 1.0327955) < 0.001f32);
        assert!(!boolean_scorer.advance());
    }
    
    
    #[test]
    pub fn test_term_scorer() {
        let left_fieldnorms = U32FastFieldReader::from(vec!(10, 4));
        assert_eq!(left_fieldnorms.get(0), 10);
        assert_eq!(left_fieldnorms.get(1), 4);
        let left = segment_postings_test_factory.from_data(vec!(1));
        let mut left_scorer = TermScorer {
            idf: 0.30685282, // 1f32,
            fieldnorm_reader: left_fieldnorms,
            segment_postings: left,
        };
        left_scorer.advance();
        assert!(abs_diff(left_scorer.score(), 0.15342641) < 0.001f32);
    }

}