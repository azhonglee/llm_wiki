fn main() {
    if let Err(error) = llm_wiki_server::run_from_env() {
        eprintln!("llm-wiki-server: {error}");
        std::process::exit(1);
    }
}
