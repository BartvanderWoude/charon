use charon::{prolog, web};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let persist = args.iter().skip(1).any(|arg| arg == "--persist");
    let path = args.iter().skip(1).find(|arg| *arg != "--persist");

    let Some(path) = path else {
        eprintln!("Usage: {} [--persist] <knowledge_base_file>", args[0]);
        return;
    };

    let knowledge_base = match prolog::KnowledgeBase::load(path) {
        Ok(kb) => kb,
        Err(error) => {
            eprintln!("{}", error);
            return;
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let interpreter = knowledge_base.parse();
        let mut api = web::Api::new(
            knowledge_base.predicates(),
            interpreter,
            knowledge_base.module_doc(),
        );
        if persist {
            api = api.with_persistence(knowledge_base);
        }

        let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
            .await
            .expect("failed to bind to 0.0.0.0:3000");
        println!(
            "Listening on http://{}",
            listener
                .local_addr()
                .expect("bound listener has an address")
        );
        axum::serve(listener, api.routes())
            .await
            .expect("server error");
    })
}
