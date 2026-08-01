use regex::Regex;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    Input,       // +
    Output,      // -
    InputOutput, // ?
    ReadOnly,    // @
}

impl FromStr for Mode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "+" => Ok(Mode::Input),
            "-" => Ok(Mode::Output),
            "?" => Ok(Mode::InputOutput),
            "@" => Ok(Mode::ReadOnly),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Input => write!(f, "Input (+)"),
            Mode::Output => write!(f, "Output (-)"),
            Mode::InputOutput => write!(f, "Input/Output (?)"),
            Mode::ReadOnly => write!(f, "Read-Only (@)"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Type {
    Any,
    Atom,
    Integer,
    Float,
    List,
    Compound,
    Stream,
    Custom(String),
}

impl FromStr for Type {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "any" => Ok(Type::Any),
            "atom" => Ok(Type::Atom),
            "int" | "integer" => Ok(Type::Integer),
            "float" | "number" => Ok(Type::Float),
            "list" => Ok(Type::List),
            "compound" => Ok(Type::Compound),
            "stream" => Ok(Type::Stream),
            other => Ok(Type::Custom(other.to_string())),
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Any => write!(f, "any"),
            Type::Atom => write!(f, "atom"),
            Type::Integer => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::List => write!(f, "list"),
            Type::Compound => write!(f, "compound"),
            Type::Stream => write!(f, "stream"),
            Type::Custom(custom) => write!(f, "{}", custom),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Argument {
    pub name: String,
    pub mode: Option<Mode>,
    pub param_type: Type,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Predicate {
    pub name: String,
    pub arguments: Vec<Argument>,
}

impl Predicate {
    pub(super) fn parse(source_code: &str) -> Vec<Predicate> {
        // Matches lines starting with "%!" and captures the signature body
        let doc_line_re = Regex::new(r"(?m)^\s*%\!\s+([a-zA-Z0-9_]+)\s*\(([^)]*)\)").unwrap();

        // Matches mode (+/-/?/@), variable name, and optional :type
        let arg_re =
            Regex::new(r"([+\-?@])?\s*([a-zA-Z0-9_]+)(?:\s*:\s*([a-zA-Z0-9_]+))?").unwrap();

        doc_line_re
            .captures_iter(source_code)
            .map(|cap| {
                let pred_name = cap[1].to_string();
                let args_str = &cap[2];
                let arguments = arg_re
                    .captures_iter(args_str)
                    .map(|arg_cap| {
                        let mode = arg_cap.get(1).and_then(|m| m.as_str().parse::<Mode>().ok());
                        let name = arg_cap[2].to_string();
                        let param_type = arg_cap
                            .get(3)
                            .and_then(|t| t.as_str().parse::<Type>().ok())
                            .unwrap_or(Type::Any);

                        Argument {
                            name,
                            mode,
                            param_type,
                        }
                    })
                    .collect();

                Predicate {
                    name: pred_name,
                    arguments,
                }
            })
            .collect()
    }
}

// ============================================================================
// UNIT TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_from_str() {
        assert_eq!("+".parse::<Mode>(), Ok(Mode::Input));
        assert_eq!("-".parse::<Mode>(), Ok(Mode::Output));
        assert_eq!("?".parse::<Mode>(), Ok(Mode::InputOutput));
        assert_eq!("@".parse::<Mode>(), Ok(Mode::ReadOnly));
        assert!("invalid".parse::<Mode>().is_err());
    }

    #[test]
    fn test_type_from_str() {
        assert_eq!("int".parse::<Type>(), Ok(Type::Integer));
        assert_eq!("integer".parse::<Type>(), Ok(Type::Integer));
        assert_eq!("float".parse::<Type>(), Ok(Type::Float));
        assert_eq!("number".parse::<Type>(), Ok(Type::Float));
        assert_eq!("atom".parse::<Type>(), Ok(Type::Atom));
        assert_eq!("list".parse::<Type>(), Ok(Type::List));
        assert_eq!("any".parse::<Type>(), Ok(Type::Any));

        // Custom types
        assert_eq!(
            "my_custom_type".parse::<Type>(),
            Ok(Type::Custom("my_custom_type".to_string()))
        );
    }

    #[test]
    fn test_parse_basic_pldoc() {
        let code = "%! length(+List:list, -Length:int) is det.";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "length");
        assert_eq!(
            res[0].arguments,
            vec![
                Argument {
                    name: "List".to_string(),
                    mode: Some(Mode::Input),
                    param_type: Type::List,
                },
                Argument {
                    name: "Length".to_string(),
                    mode: Some(Mode::Output),
                    param_type: Type::Integer,
                }
            ]
        );
    }

    #[test]
    fn test_parse_predicate_zero_args() {
        let code = "%! true_pred()";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "true_pred");
        assert!(res[0].arguments.is_empty());
    }

    #[test]
    fn test_parse_all_modes_and_unspecified_types() {
        let code = "%! mix_modes(+In, -Out, ?InOut, @Read)";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(
            res[0].arguments,
            vec![
                Argument {
                    name: "In".to_string(),
                    mode: Some(Mode::Input),
                    param_type: Type::Any,
                },
                Argument {
                    name: "Out".to_string(),
                    mode: Some(Mode::Output),
                    param_type: Type::Any,
                },
                Argument {
                    name: "InOut".to_string(),
                    mode: Some(Mode::InputOutput),
                    param_type: Type::Any,
                },
                Argument {
                    name: "Read".to_string(),
                    mode: Some(Mode::ReadOnly),
                    param_type: Type::Any,
                }
            ]
        );
    }

    #[test]
    fn test_ignore_false_positives_in_strings() {
        let code = r#"
%! valid(+Arg:atom)
valid(Arg) :-
    format("Not a docstring %! fake(+X:int)"),
    assertz(data("%! another_fake()")).
"#;
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "valid");
    }

    #[test]
    fn test_whitespace_and_formatting_variations() {
        let code = "  %!   spaced_out  (  +  Var  :  atom   ,   -  Out  :  int  )  ";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "spaced_out");
        assert_eq!(
            res[0].arguments,
            vec![
                Argument {
                    name: "Var".to_string(),
                    mode: Some(Mode::Input),
                    param_type: Type::Atom,
                },
                Argument {
                    name: "Out".to_string(),
                    mode: Some(Mode::Output),
                    param_type: Type::Integer,
                }
            ]
        );
    }

    #[test]
    fn test_custom_domain_types() {
        let code = "%! process_order(+Order:order_struct, -Receipt:json_dict)";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(
            res[0].arguments[0],
            Argument {
                name: "Order".to_string(),
                mode: Some(Mode::Input),
                param_type: Type::Custom("order_struct".to_string()),
            }
        );
        assert_eq!(
            res[0].arguments[1],
            Argument {
                name: "Receipt".to_string(),
                mode: Some(Mode::Output),
                param_type: Type::Custom("json_dict".to_string()),
            }
        );
    }

    #[test]
    fn test_multiple_predicates_in_one_file() {
        let code = r#"
%! pred_one(+X:int)
pred_one(X).

% Standard Prolog comment
% pred_one is great!

%! pred_two(-Y:atom)
pred_two(Y).
"#;
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 2);
        assert_eq!(res[0].name, "pred_one");
        assert_eq!(res[1].name, "pred_two");
    }
}
