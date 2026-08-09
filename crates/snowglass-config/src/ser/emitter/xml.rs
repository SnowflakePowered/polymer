use std::io::Write;

use quick_xml::{
    Writer,
    events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event},
};

use crate::error::Error;

use super::{DocumentRef, EmitResult, EmittedBinding, EmittedValue, Emitter, SectionRef};

/// An in-memory XML emitter backed by [`quick_xml`].
pub struct XmlEmitter {
    document: Writer<Vec<u8>>,
    sections: Vec<Box<str>>,
    roots: usize,
}

impl XmlEmitter {
    pub fn new() -> Self {
        Self::with_indent(b' ', 2)
    }

    pub fn with_indent(indent: u8, indent_size: usize) -> Self {
        Self {
            document: Writer::new_with_indent(Vec::new(), indent, indent_size),
            sections: Vec::new(),
            roots: 0,
        }
    }

    pub fn document(&self) -> &Writer<Vec<u8>> {
        &self.document
    }
}

impl Default for XmlEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl Emitter for XmlEmitter {
    fn begin_document(&mut self, _document: DocumentRef<'_>) -> EmitResult<()> {
        self.document
            .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
        Ok(())
    }

    fn enter_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        if self.sections.is_empty() {
            self.roots += 1;
            if self.roots > 1 {
                return Err(Error::emitter_message(
                    "an XML document cannot contain more than one root element",
                ));
            }
        }
        self.document
            .write_event(Event::Start(BytesStart::new(section.name)))?;
        self.sections.push(section.name.into());
        Ok(())
    }

    fn write_value(&mut self, value: EmittedValue<'_>) -> EmitResult<()> {
        if self.sections.is_empty() {
            return Err(Error::emitter_message(
                "an XML value cannot be emitted outside the root element",
            ));
        }
        let rendered = value.value.render()?;
        self.document
            .create_element(value.key)
            .write_text_content(BytesText::new(&rendered))?;
        Ok(())
    }

    fn write_binding(&mut self, binding: EmittedBinding<'_>) -> EmitResult<()> {
        if self.sections.is_empty() {
            return Err(Error::emitter_message(
                "an XML binding cannot be emitted outside the root element",
            ));
        }
        self.document
            .write_event(Event::Start(BytesStart::new("entry")))?;
        self.document
            .create_element("mapping")
            .write_text_content(BytesText::new(binding.key))?;
        self.document
            .create_element("button")
            .write_text_content(BytesText::new(&binding.value))?;
        self.document
            .write_event(Event::End(BytesEnd::new("entry")))?;
        Ok(())
    }

    fn exit_section(&mut self, section: SectionRef<'_>) -> EmitResult<()> {
        let entered = self.sections.pop().ok_or_else(|| {
            Error::emitter_message("exited an XML element that was never entered")
        })?;
        if entered.as_ref() != section.name {
            return Err(Error::emitter_message(format!(
                "exited XML element `{}` while `{entered}` was active",
                section.name
            )));
        }
        self.document
            .write_event(Event::End(BytesEnd::new(entered.as_ref())))?;
        Ok(())
    }

    fn finish(&mut self, writer: &mut dyn Write) -> EmitResult<()> {
        if let Some(section) = self.sections.last() {
            return Err(Error::emitter_message(format!(
                "XML element `{section}` was not exited"
            )));
        }
        if self.roots != 1 {
            return Err(Error::emitter_message(
                "an XML document must contain exactly one root element",
            ));
        }
        writer.write_all(self.document.get_ref())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use quick_xml::{Reader, events::Event};

    use super::*;

    #[test]
    fn builds_nested_xml_and_escapes_text() {
        let mut emitter = Box::new(XmlEmitter::new());
        emitter
            .begin_document(DocumentRef {
                target: "settings",
                output: &crate::ast::TargetOutputDefinition {
                    format: "xml".into(),
                    path: "settings.xml".into(),
                },
            })
            .unwrap();
        let content = SectionRef {
            path: "settings.content",
            name: "content",
        };
        let graphic = SectionRef {
            path: "settings.content.Graphic",
            name: "Graphic",
        };
        emitter.enter_section(content).unwrap();
        emitter.enter_section(graphic).unwrap();
        emitter
            .write_value(EmittedValue {
                key: "device",
                value: super::super::ScalarValue::String("A&B < Vulkan"),
            })
            .unwrap();
        emitter.exit_section(graphic).unwrap();
        emitter.exit_section(content).unwrap();
        let mut bytes = Vec::new();
        emitter.finish(&mut bytes).unwrap();
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("A&amp;B &lt; Vulkan")
        );

        let mut reader = Reader::from_reader(bytes.as_slice());
        loop {
            if reader.read_event().unwrap() == Event::Eof {
                break;
            }
        }
    }
}
