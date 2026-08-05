use charon::{prolog, web};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <knowledge_base_file>", args[0]);
        return;
    }

    let knowledge_base =
        match prolog::KnowledgeBase::load(args.get(1).map(|s| s.as_str()).expect("valid path")) {
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
        let api = web::Api::new(knowledge_base.predicates(), interpreter);

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
