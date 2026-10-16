//! Prints a summary of a GGUF file: `cargo run -p alkahest-gguf --example dump -- <path>`.

fn main() {
    let Some(p) = std::env::args().nth(1) else {
        return;
    };
    match alkahest_gguf::Gguf::read_file(&p) {
        Ok(g) => println!(
            "arch={:?} ctx={:?} blocks={:?} heads={:?}/{:?} ftype={:?} dom={:?} fam={:?} params={} proj={} tensors={} types={:?}",
            g.architecture(),
            g.context_length(),
            g.block_count(),
            g.head_count(),
            g.head_count_kv(),
            g.file_type_name(),
            g.dominant_quant(),
            g.quant_family(),
            g.parameter_count(),
            g.is_vision_projector(),
            g.tensors.len(),
            g.bytes_by_type()
        ),
        Err(e) => println!("error: {e}"),
    }
}
