mod prolog;

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

    println!(
        "Loaded knowledge base with {} predicates.",
        knowledge_base.predicates().len()
    );

    runtime.block_on(async move {
        let mut machine = knowledge_base.start();
        for value in machine.run_query("role(X).") {
            println!("{:?}", value);
        }
    })
}
