use std::{fs::File, io::Read, path::Path};

use scryer_prolog::{Machine, MachineBuilder};

/// A knowledge base containing Prolog and ensure compatibility with file by holding a exclusive handle.
pub struct KnowledgeBase {
    file: File,
    file_content: String,
}

impl KnowledgeBase {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .open(path.as_ref())
            .map_err(Error::Io)?;

        let mut file_content = String::new();
        file.read_to_string(&mut file_content).map_err(Error::Io)?;

        Ok(KnowledgeBase { file, file_content })
    }

    pub fn start(&self) -> Machine {
        let mut wam = MachineBuilder::default()
            .with_toplevel(scryer_prolog::default_toplevel())
            .build();
        wam.load_module_string("", &self.file_content);
        wam
    }

    /// Returns a vector of predicates defined in the knowledge base.
    pub fn predicates(&self) -> Vec<super::Predicate> {
        super::Predicate::parse(&self.file_content)
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
