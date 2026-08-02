mod prolog;
mod web;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let knowledge_base = match prolog::KnowledgeBase::load(
        args.get(1)
            .map(|s| s.as_str())
            .unwrap_or_else(|| "/workspaces/charon/association.pl"),
    ) {
        Ok(kb) => kb,
        Err(error) => {
            eprintln!("{}", error);
            return;
        }
    };

    runtime.block_on(async move {
        let interpreter = knowledge_base.parse();
        for value in interpreter.query("role(X).") {
            match value {
                Ok(json) => {
                    println!("Query result: {}", json);
                }
                Err(error) => {
                    eprintln!("Query error: {}", error);
                }
            }
        }
    })
}
