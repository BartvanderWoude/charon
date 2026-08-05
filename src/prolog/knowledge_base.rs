use std::io::{Seek, Write};
use std::{fs::File, io::Read, path::Path};

use super::Interpreter;
use super::persistence::{self, DynamicPredicate};

/// A knowledge base containing Prolog and ensure compatibility with file by holding a exclusive handle.
pub struct KnowledgeBase {
    file: File,
    file_content: String,
}

impl KnowledgeBase {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.as_ref())
            .map_err(Error::Io)?;

        let mut file_content = String::new();
        file.read_to_string(&mut file_content).map_err(Error::Io)?;

        Ok(KnowledgeBase { file, file_content })
    }

    pub fn parse(&self) -> Interpreter {
        Interpreter::load(&self.file_content)
    }

    /// Returns a vector of predicates defined in the knowledge base.
    pub fn predicates(&self) -> Vec<super::Predicate> {
        super::Predicate::parse(&self.file_content)
    }

    /// Returns the knowledge base's file-level PlDoc module comment, if it has one.
    pub fn module_doc(&self) -> Option<super::ModuleDoc> {
        super::ModuleDoc::parse(&self.file_content)
    }

    /// Every predicate the knowledge base declares `dynamic` (via `:- dynamic(name/arity).`),
    /// i.e. every predicate `persist` can write back to the source file.
    pub fn dynamic_predicates(&self) -> Vec<DynamicPredicate> {
        persistence::parse_dynamic_directives(&self.file_content)
    }

    /// Mirrors `predicate`'s current facts — as they stand in `interpreter`'s live machine —
    /// back into this knowledge base's in-memory source text: facts that are now true but have
    /// no matching source line are appended (right after the predicate's last existing fact
    /// line, or at end of file if it had none), and source lines whose fact is no longer true
    /// are removed. Only simple ground fact lines are recognized (see
    /// `persistence::is_fact_line`) — rule clauses and anything else are left untouched.
    ///
    /// This only updates the in-memory copy of the source; call `persist` to also write it to
    /// disk. Returns whether anything changed.
    fn sync(&mut self, interpreter: &Interpreter, predicate: &DynamicPredicate) -> bool {
        let live = persistence::live_facts(interpreter, predicate);

        let lines: Vec<&str> = self.file_content.lines().collect();
        let fact_line_indices: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| persistence::is_fact_line(line, predicate))
            .map(|(i, _)| i)
            .collect();

        let mut live_remaining = live;
        let mut kept_indices = Vec::new();
        for &i in &fact_line_indices {
            let term_text = lines[i].trim().trim_end_matches('.');
            let Some(canonical) = persistence::canonicalize(interpreter, term_text) else {
                continue;
            };
            if let Some(pos) = live_remaining.iter().position(|fact| *fact == canonical) {
                live_remaining.remove(pos);
                kept_indices.push(i);
            }
        }

        let any_removed = kept_indices.len() != fact_line_indices.len();
        if !any_removed && live_remaining.is_empty() {
            return false;
        }

        let insert_after = fact_line_indices.iter().copied().max();

        let mut new_lines: Vec<String> = Vec::with_capacity(lines.len() + live_remaining.len());
        for (i, line) in lines.iter().enumerate() {
            if fact_line_indices.contains(&i) && !kept_indices.contains(&i) {
                continue;
            }
            new_lines.push(line.to_string());
            if insert_after == Some(i) {
                new_lines.extend(live_remaining.iter().map(|fact| format!("{fact}.")));
            }
        }
        if insert_after.is_none() {
            new_lines.extend(live_remaining.iter().map(|fact| format!("{fact}.")));
        }

        self.file_content = new_lines.join("\n");
        if !self.file_content.ends_with('\n') {
            self.file_content.push('\n');
        }
        true
    }

    /// Syncs every one of `predicates`' facts (see `sync`) against `interpreter`'s current state
    /// and, if anything changed, writes the updated source back to this knowledge base's file.
    pub fn persist(
        &mut self,
        interpreter: &Interpreter,
        predicates: &[DynamicPredicate],
    ) -> Result<(), Error> {
        let mut changed = false;
        for predicate in predicates {
            changed |= self.sync(interpreter, predicate);
        }
        if changed {
            self.file.set_len(0).map_err(Error::Io)?;
            self.file.rewind().map_err(Error::Io)?;
            self.file
                .write_all(self.file_content.as_bytes())
                .map_err(Error::Io)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(error) => write!(f, "io error: {}", error),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use json::JsonValue;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_pl(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "charon_kb_test_{name}_{}_{}.pl",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn persist_appends_new_fact_and_removes_retracted() {
        let path = temp_pl(
            "assert_retract",
            ":- dynamic(example/1).\nexample(a).\nexample(b).\n",
        );

        let mut kb = KnowledgeBase::load(&path).unwrap();
        let interpreter = kb.parse();
        let dynamic_predicates = kb.dynamic_predicates();
        assert_eq!(
            dynamic_predicates,
            vec![DynamicPredicate {
                name: "example".to_string(),
                arity: 1
            }]
        );

        let results = interpreter.query("assertz(example(c)), example(a), retract(example(a)).");
        assert!(matches!(results.as_slice(), [Ok(_)]));

        kb.persist(&interpreter, &dynamic_predicates).unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("example(a)."), "{on_disk}");
        assert!(on_disk.contains("example(b)."), "{on_disk}");
        assert!(on_disk.contains("example(c)."), "{on_disk}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn persist_writes_asserts_even_when_overall_query_fails() {
        let path = temp_pl(
            "failing_query",
            ":- dynamic(example/1).\nexample(a).\nexample(b).\n",
        );

        let mut kb = KnowledgeBase::load(&path).unwrap();
        let interpreter = kb.parse();
        let dynamic_predicates = kb.dynamic_predicates();

        // The overall query fails (it ends in `fail`), but the assertz/retract before it already
        // committed to the machine and should still be persisted.
        let results =
            interpreter.query("assertz(example(c)), example(a), retract(example(a)), fail.");
        assert_eq!(results, vec![Ok(JsonValue::Boolean(false))]);

        kb.persist(&interpreter, &dynamic_predicates).unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("example(a)."), "{on_disk}");
        assert!(on_disk.contains("example(b)."), "{on_disk}");
        assert!(on_disk.contains("example(c)."), "{on_disk}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn persist_is_a_noop_when_nothing_changed() {
        let path = temp_pl("noop", ":- dynamic(example/1).\nexample(a).\nexample(b).\n");

        let mut kb = KnowledgeBase::load(&path).unwrap();
        let interpreter = kb.parse();
        let dynamic_predicates = kb.dynamic_predicates();

        interpreter.query("example(a).");

        let before = std::fs::read_to_string(&path).unwrap();
        kb.persist(&interpreter, &dynamic_predicates).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn persist_preserves_quoting_and_survives_multiple_requests() {
        let path = temp_pl(
            "quoting_multi_request",
            ":- dynamic(role/1).\nrole(\"Member\").\n",
        );

        let mut kb = KnowledgeBase::load(&path).unwrap();
        let interpreter = kb.parse();
        let dynamic_predicates = kb.dynamic_predicates();

        // First "request": assert a new fact.
        interpreter.query("assertz(role(\"Chairperson\")).");
        kb.persist(&interpreter, &dynamic_predicates).unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        assert!(after_first.contains("role(\"Member\")."), "{after_first}");
        assert!(
            after_first.contains("role(\"Chairperson\")."),
            "{after_first}"
        );

        // Second "request", against a freshly reloaded knowledge base/interpreter (as would
        // happen across separate server runs): retract the fact the first request added.
        let mut kb = KnowledgeBase::load(&path).unwrap();
        let interpreter = kb.parse();
        let dynamic_predicates = kb.dynamic_predicates();
        interpreter.query("retract(role(\"Chairperson\")).");
        kb.persist(&interpreter, &dynamic_predicates).unwrap();

        let after_second = std::fs::read_to_string(&path).unwrap();
        assert!(after_second.contains("role(\"Member\")."), "{after_second}");
        assert!(
            !after_second.contains("role(\"Chairperson\")."),
            "{after_second}"
        );

        std::fs::remove_file(&path).ok();
    }
}
