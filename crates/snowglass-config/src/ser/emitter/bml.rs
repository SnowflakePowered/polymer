use std::{fmt::Write as _, io::Write};

use bml::BmlNode;

use crate::error::Error;

use super::{EmitResult, EmittedValue, Emitter, SectionRef};

/// An in-memory BML emitter validated and serialized by the `bml` crate.
#[derive(Default)]
pub struct BmlEmitter {
    nodes: Vec<Node>,
    roots: Vec<usize>,
    sections: Vec<usize>,
}

struct Node {
    name: Box<str>,
    value: Option<String>,
    children: Vec<usize>,
}

impl BmlEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    fn append(&mut self, name: &str, value: Option<String>) -> usize {
        let handle = self.nodes.len();
        self.nodes.push(Node {
            name: name.into(),
            value,
            children: Vec::new(),
        });
        if let Some(parent) = self.sections.last().copied() {
            self.nodes[parent].children.push(handle);
        } else {
            self.roots.push(handle);
        }
        handle
    }

    fn source(&self) -> Result<String, std::fmt::Error> {
        fn write_node(
            output: &mut String,
            nodes: &[Node],
            handle: usize,
            depth: usize,
        ) -> std::fmt::Result {
            let node = &nodes[handle];
            let indent = "  ".repeat(depth);
            match node.value.as_deref() {
                None => writeln!(output, "{indent}{}", node.name)?,
                Some(value) if !value.contains(['\r', '\n']) => {
                    writeln!(output, "{indent}{}: {value}", node.name)?;
                }
                Some(value) => {
                    writeln!(output, "{indent}{}", node.name)?;
                    for line in value.lines() {
                        writeln!(output, "{indent}  :{line}")?;
                    }
                }
            }
            for child in &node.children {
                write_node(output, nodes, *child, depth + 1)?;
            }
            Ok(())
        }

        let mut output = String::new();
        for root in &self.roots {
            write_node(&mut output, &self.nodes, *root, 0)?;
        }
        Ok(output)
    }
}

impl Emitter for BmlEmitter {
    fn enter_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        let handle = self.append(section.name, None);
        self.sections.push(handle);
        Ok(())
    }

    fn write_value(&mut self, value: EmittedValue<'_>) -> EmitResult<()> {
        self.append(value.key, Some(value.value.render()?.into_owned()));
        Ok(())
    }

    fn exit_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        let handle = self
            .sections
            .pop()
            .ok_or_else(|| Error::emitter_message("exited a BML section that was never entered"))?;
        let entered = &self.nodes[handle].name;
        if entered.as_ref() != section.name {
            return Err(Error::emitter_message(format!(
                "exited BML section `{}` while `{entered}` was active",
                section.name
            )));
        }
        Ok(())
    }

    fn finish(&mut self, writer: &mut dyn Write) -> EmitResult<()> {
        if let Some(handle) = self.sections.last() {
            return Err(Error::emitter_message(format!(
                "BML section `{}` was not exited",
                self.nodes[*handle].name
            )));
        }
        let source = self.source().map_err(Error::emitter)?;
        let document = BmlNode::try_from(source.as_str()).map_err(Error::emitter)?;
        writer.write_all(document.to_string().as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::ser::{EmittedValue, ScalarValue};

    use super::*;

    #[test]
    fn builds_nested_bml_through_the_crate_model() {
        let mut emitter = BmlEmitter::new();
        let paths = SectionRef {
            path: "settings.Paths",
            name: "Paths",
        };
        let super_famicom = SectionRef {
            path: "settings.Paths.SuperFamicom",
            name: "SuperFamicom",
        };
        emitter.enter_section(paths).unwrap();
        emitter
            .write_value(EmittedValue {
                key: "Home",
                value: ScalarValue::String("/games/ares library"),
            })
            .unwrap();
        emitter.enter_section(super_famicom).unwrap();
        emitter
            .write_value(EmittedValue {
                key: "BSMemory",
                value: ScalarValue::String("/games/bs"),
            })
            .unwrap();
        emitter.exit_section(super_famicom).unwrap();
        emitter.exit_section(paths).unwrap();

        let mut bytes = Vec::new();
        emitter.finish(&mut bytes).unwrap();
        let output = String::from_utf8(bytes).unwrap();
        let parsed = BmlNode::try_from(output.as_str()).unwrap();
        let paths = parsed.named("Paths").next().unwrap();
        assert_eq!(
            paths.named("Home").next().unwrap().value(),
            "/games/ares library"
        );
        assert_eq!(
            paths
                .named("SuperFamicom")
                .next()
                .unwrap()
                .named("BSMemory")
                .next()
                .unwrap()
                .value(),
            "/games/bs"
        );
    }
}
