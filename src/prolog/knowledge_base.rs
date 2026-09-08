//! A `.pl` file: its text, what that text documents, and how runtime changes get written back.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::persistence::{self, DynamicPredicate};
use super::{Interpreter, ModuleDoc, Predicate};

/// A knowledge base's source text, together with the path it came from.
///
/// Nothing here holds the file open. An earlier version kept a read-write handle "to claim
/// exclusivity", which it did not — no lock was ever taken — while making a read-only `.pl` file
/// impossible to serve at all. Writes now go through [`replace_atomically`] instead, so a
/// concurrent editor is the worst case rather than a truncated knowledge base.
#[derive(Debug, Clone)]
pub struct KnowledgeBase {
    path: PathBuf,
    source: String,
}

impl KnowledgeBase {
    /// Reads `path` into memory.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|cause| Error::read(path, cause))?;
        Ok(KnowledgeBase {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The file this knowledge base was read from, so it can be re-read on reload.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The source text, as loaded. Exposed so it can be served as an MCP resource: a model that
    /// can read the rules it is calling does not have to take the tool descriptions on faith.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Consults the source into a fresh interpreter.
    pub fn parse(&self) -> Interpreter {
        Interpreter::load(&self.source)
    }

    /// The predicates the source documents with a `%!` PlDoc comment.
    pub fn predicates(&self) -> Vec<Predicate> {
        Predicate::parse(&self.source)
    }

    /// The file-level PlDoc module comment, if it has one.
    pub fn module_doc(&self) -> Option<ModuleDoc> {
        ModuleDoc::parse(&self.source)
    }

    /// Every predicate the source declares `dynamic`, i.e. every predicate [`Self::persist`] can
    /// write back.
    pub fn dynamic_predicates(&self) -> Vec<DynamicPredicate> {
        persistence::parse_dynamic_directives(&self.source)
    }

    /// Checks now that persisting later will be able to write, so `--persist` against a
    /// read-only file fails at startup rather than after the first `assertz`.
    pub fn ensure_writable(&self) -> Result<(), Error> {
        fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map(drop)
            .map_err(|cause| Error::write(&self.path, cause))
    }

    /// Mirrors every one of `predicates`' facts, as they stand in `interpreter`'s live machine,
    /// into the source text and — if anything changed — back onto disk. Returns whether the file
    /// was rewritten.
    pub fn persist(
        &mut self,
        interpreter: &Interpreter,
        predicates: &[DynamicPredicate],
    ) -> Result<bool, Error> {
        let mut changed = false;
        for predicate in predicates {
            changed |= self.sync(interpreter, predicate);
        }
        if changed {
            replace_atomically(&self.path, &self.source)
                .map_err(|cause| Error::write(&self.path, cause))?;
        }
        Ok(changed)
    }

    /// Brings the in-memory source text into line with `predicate`'s live facts: facts that are
    /// now true but have no matching source line are appended after the predicate's last
    /// existing fact line (or at end of file), and lines whose fact is no longer true are
    /// removed. Returns whether anything changed.
    ///
    /// Only lines [`persistence::is_fact_line`] recognises are touched. A line that is a fact of
    /// this predicate but cannot be read back as a term is *kept*: an unparseable line is a
    /// reason to leave the author's text alone, not to delete it.
    fn sync(&mut self, interpreter: &Interpreter, predicate: &DynamicPredicate) -> bool {
        let lines: Vec<&str> = self.source.lines().collect();
        let fact_lines: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| persistence::is_fact_line(line, predicate))
            .map(|(index, _)| index)
            .collect();

        let mut unmatched = persistence::live_facts(interpreter, predicate);
        let mut kept: HashSet<usize> = HashSet::new();
        for &index in &fact_lines {
            let term = lines[index].trim().trim_end_matches('.');
            let Some(canonical) = persistence::canonicalize(interpreter, term) else {
                kept.insert(index);
                continue;
            };
            if let Some(position) = unmatched.iter().position(|fact| *fact == canonical) {
                unmatched.remove(position);
                kept.insert(index);
            }
        }

        if unmatched.is_empty() && kept.len() == fact_lines.len() {
            return false;
        }

        // Appended after the predicate's last fact line even when that line is itself being
        // removed — skipping the insertion along with the removal is how a retract-then-assert
        // in one request used to lose the asserted fact.
        let insert_after = fact_lines.last().copied();
        let mut rewritten: Vec<String> = Vec::with_capacity(lines.len() + unmatched.len());
        let mut appended = false;
        for (index, line) in lines.iter().enumerate() {
            if !fact_lines.contains(&index) || kept.contains(&index) {
                rewritten.push((*line).to_string());
            }
            if insert_after == Some(index) {
                rewritten.extend(unmatched.iter().map(|fact| format!("{fact}.")));
                appended = true;
            }
        }
        if !appended {
            rewritten.extend(unmatched.iter().map(|fact| format!("{fact}.")));
        }

        self.source = rewritten.join("\n");
        if !self.source.ends_with('\n') {
            self.source.push('\n');
        }
        true
    }
}

/// Writes `contents` to `path` by way of a sibling temporary file and a rename.
///
/// Truncating the real file and writing into it — as this used to — means a crash, a full disk,
/// or a signal between the two leaves the knowledge base empty or half-written. A rename is
/// atomic on every platform Charon targets, so a reader either sees the old file or the new one.
fn replace_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let directory = directory.unwrap_or_else(|| Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = directory.join(format!(".{name}.charon-{}.tmp", std::process::id()));

    let written = (|| -> io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()
    })();
    if let Err(cause) = written {
        let _ = fs::remove_file(&temporary);
        return Err(cause);
    }

    // Best-effort: a knowledge base that was mode 0600 should not become 0644 on first write.
    if let Ok(metadata) = fs::metadata(path) {
        let _ = fs::set_permissions(&temporary, metadata.permissions());
    }

    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

/// Why a knowledge base could not be read or written.
#[derive(Debug)]
pub enum Error {
    Read { path: PathBuf, cause: io::Error },
    Write { path: PathBuf, cause: io::Error },
}

impl Error {
    fn read(path: &Path, cause: io::Error) -> Self {
        Error::Read {
            path: path.to_path_buf(),
            cause,
        }
    }

    fn write(path: &Path, cause: io::Error) -> Self {
        Error::Write {
            path: path.to_path_buf(),
            cause,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Read { path, cause } => {
                write!(f, "could not read `{}`: {cause}", path.display())
            }
            Error::Write { path, cause } => {
                write!(f, "could not write `{}`: {cause}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Read { cause, .. } | Error::Write { cause, .. } => Some(cause),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, contents: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "charon_kb_{name}_{}_{}.pl",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            fs::write(&path, contents).unwrap();
            TempFile(path)
        }

        fn read(&self) -> String {
            fs::read_to_string(&self.0).unwrap()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    /// Loads a knowledge base and its interpreter, runs `goal`, then persists.
    fn round_trip(file: &TempFile, goal: &str) -> String {
        let mut knowledge_base = KnowledgeBase::load(&file.0).unwrap();
        let interpreter = knowledge_base.parse();
        let dynamic = knowledge_base.dynamic_predicates();
        interpreter.query(goal).expect("query should not raise");
        knowledge_base.persist(&interpreter, &dynamic).unwrap();
        file.read()
    }

    #[test]
    fn appends_new_facts_and_removes_retracted_ones() {
        let file = TempFile::new(
            "assert_retract",
            ":- dynamic(example/1).\nexample(a).\nexample(b).\n",
        );
        let on_disk = round_trip(&file, "assertz(example(c)), retract(example(a)).");

        assert!(!on_disk.contains("example(a)."), "{on_disk}");
        assert!(on_disk.contains("example(b)."), "{on_disk}");
        assert!(on_disk.contains("example(c)."), "{on_disk}");
    }

    #[test]
    fn an_assert_after_removing_the_last_fact_line_is_still_written() {
        // The insertion point is the predicate's last fact line. When that very line is the one
        // being retracted, the append used to be skipped along with it and the new fact
        // vanished — visibly only when the file happened to hold exactly one fact.
        let file = TempFile::new("append_after_removal", ":- dynamic(only/1).\nonly(a).\n");
        let on_disk = round_trip(&file, "retract(only(a)), assertz(only(b)).");

        assert!(!on_disk.contains("only(a)."), "{on_disk}");
        assert!(on_disk.contains("only(b)."), "{on_disk}");
    }

    #[test]
    fn writes_asserts_even_when_the_overall_query_fails() {
        let file = TempFile::new("failing_query", ":- dynamic(example/1).\nexample(a).\n");
        // `assertz` commits to the machine immediately; the trailing `fail` does not undo it.
        let on_disk = round_trip(&file, "assertz(example(c)), fail.");

        assert!(on_disk.contains("example(c)."), "{on_disk}");
    }

    #[test]
    fn is_a_noop_when_nothing_changed() {
        let file = TempFile::new("noop", ":- dynamic(example/1).\nexample(a).\nexample(b).\n");
        let before = file.read();
        let after = round_trip(&file, "example(a).");
        assert_eq!(before, after);
    }

    #[test]
    fn leaves_clauses_of_a_different_arity_alone() {
        // `example/2` is nobody's business here: `:- dynamic(example/1).` says nothing about it,
        // and a source line matching only on the functor used to get it deleted.
        let file = TempFile::new(
            "other_arity",
            ":- dynamic(example/1).\nexample(a).\nexample(a, b).\n",
        );
        let on_disk = round_trip(&file, "assertz(example(c)).");

        assert!(on_disk.contains("example(a, b)."), "{on_disk}");
        assert!(on_disk.contains("example(c)."), "{on_disk}");
    }

    #[test]
    fn leaves_rules_alone() {
        let file = TempFile::new(
            "rules",
            ":- dynamic(example/1).\nexample(a).\nexample(X) :- other(X).\nother(z).\n",
        );
        let on_disk = round_trip(&file, "assertz(example(c)).");

        assert!(on_disk.contains("example(X) :- other(X)."), "{on_disk}");
    }

    #[test]
    fn preserves_quoting_across_separate_runs() {
        let file = TempFile::new("quoting", ":- dynamic(role/1).\nrole(\"Member\").\n");

        let after_first = round_trip(&file, "assertz(role(\"Chairperson\")).");
        assert!(after_first.contains("role(\"Member\")."), "{after_first}");
        assert!(
            after_first.contains("role(\"Chairperson\")."),
            "{after_first}"
        );

        // A second run against a freshly reloaded file, as would happen across server restarts.
        let after_second = round_trip(&file, "retract(role(\"Chairperson\")).");
        assert!(after_second.contains("role(\"Member\")."), "{after_second}");
        assert!(
            !after_second.contains("role(\"Chairperson\")."),
            "{after_second}"
        );
    }

    #[test]
    fn persist_reports_whether_it_wrote() {
        let file = TempFile::new("reports", ":- dynamic(example/1).\nexample(a).\n");
        let mut knowledge_base = KnowledgeBase::load(&file.0).unwrap();
        let interpreter = knowledge_base.parse();
        let dynamic = knowledge_base.dynamic_predicates();

        assert!(!knowledge_base.persist(&interpreter, &dynamic).unwrap());
        interpreter.query("assertz(example(b)).").unwrap();
        assert!(knowledge_base.persist(&interpreter, &dynamic).unwrap());
        assert!(!knowledge_base.persist(&interpreter, &dynamic).unwrap());
    }

    #[test]
    fn a_read_only_knowledge_base_still_loads() {
        // Serving a `.pl` file nobody may write is entirely reasonable; opening it read-write at
        // load time made it impossible.
        let file = TempFile::new("read_only", "example(a).\n");
        let mut permissions = fs::metadata(&file.0).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file.0, permissions).unwrap();

        let knowledge_base = KnowledgeBase::load(&file.0).expect("read-only files are servable");
        assert!(knowledge_base.source().contains("example(a)."));
        // …but asking to persist to it fails up front rather than after the first request.
        assert!(knowledge_base.ensure_writable().is_err());

        let mut permissions = fs::metadata(&file.0).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&file.0, permissions).unwrap();
    }

    #[test]
    fn a_missing_file_names_itself_in_the_error() {
        let error = KnowledgeBase::load("/nonexistent/charon/kb.pl").unwrap_err();
        assert!(error.to_string().contains("/nonexistent/charon/kb.pl"));
    }
}
