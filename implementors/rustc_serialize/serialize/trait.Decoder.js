(function() {var implementors = {};
implementors["bincode"] = ["impl&lt;'a, R:&nbsp;<a class="trait" href="https://doc.rust-lang.org/nightly/std/io/trait.Read.html" title="trait std::io::Read">Read</a>&gt; <a class="trait" href="rustc_serialize/serialize/trait.Decoder.html" title="trait rustc_serialize::serialize::Decoder">Decoder</a> for <a class="struct" href="bincode/rustc_serialize/struct.DecoderReader.html" title="struct bincode::rustc_serialize::DecoderReader">DecoderReader</a>&lt;'a, R&gt;",];
implementors["tantivy"] = ["impl&lt;'a, R&gt; <a class="trait" href="rustc_serialize/serialize/trait.Decoder.html" title="trait rustc_serialize::serialize::Decoder">Decoder</a> for <a class="struct" href="bincode/rustc_serialize/reader/struct.DecoderReader.html" title="struct bincode::rustc_serialize::reader::DecoderReader">DecoderReader</a>&lt;'a, R&gt; <span class="where fmt-newline">where R: <a class="trait" href="https://doc.rust-lang.org/nightly/std/io/trait.Read.html" title="trait std::io::Read">Read</a></span>",];

            if (window.register_implementors) {
                window.register_implementors(implementors);
            } else {
                window.pending_implementors = implementors;
            }
        
})()
