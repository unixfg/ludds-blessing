use crate::error::{CoreError, ErrorCode, Result};
use crate::patch::SpanPatch;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::ops::Range;

pub type ElementId = usize;

#[derive(Debug, Clone, Copy)]
pub struct XmlLimits {
    pub max_bytes: u64,
    pub max_elements: usize,
    pub max_attributes_per_element: usize,
    pub max_depth: usize,
}

impl Default for XmlLimits {
    fn default() -> Self {
        Self {
            max_bytes: 512 * 1024 * 1024,
            max_elements: 4_000_000,
            max_attributes_per_element: 128,
            max_depth: 1024,
        }
    }
}

#[derive(Debug, Clone)]
struct Attribute {
    name: String,
    value: String,
    value_range: Range<usize>,
}

#[derive(Debug, Clone)]
struct Element {
    name: String,
    parent: Option<ElementId>,
    children: Vec<ElementId>,
    attributes: Vec<Attribute>,
    open_range: Range<usize>,
    close_range: Range<usize>,
    full_range: Range<usize>,
    empty: bool,
}

/// A byte-backed XML element index with XStream-style `z`/`ref` validation.
#[derive(Debug, Clone)]
pub struct XmlDocument {
    bytes: Vec<u8>,
    elements: Vec<Element>,
    root: ElementId,
    identities: HashMap<String, ElementId>,
}

impl XmlDocument {
    pub fn parse(bytes: Vec<u8>, limits: XmlLimits) -> Result<Self> {
        if bytes.len() as u64 > limits.max_bytes {
            return Err(CoreError::new(
                ErrorCode::ResourceLimit,
                format!("XML exceeds {} bytes", limits.max_bytes),
            ));
        }
        std::str::from_utf8(&bytes)
            .map_err(|_| CoreError::new(ErrorCode::InvalidXml, "XML is not valid UTF-8"))?;
        if ascii_case_insensitive_contains(&bytes, b"<!DOCTYPE")
            || ascii_case_insensitive_contains(&bytes, b"<!ENTITY")
        {
            return Err(CoreError::new(
                ErrorCode::InvalidXml,
                "DTD and entity declarations are not allowed",
            ));
        }

        let mut reader = Reader::from_reader(Cursor::new(bytes.as_slice()));
        reader.config_mut().trim_text(false);
        reader.config_mut().check_end_names = true;
        let mut event_buf = Vec::new();
        let mut elements: Vec<Element> = Vec::new();
        let mut stack: Vec<ElementId> = Vec::new();
        let mut roots = Vec::new();

        loop {
            let before = usize::try_from(reader.buffer_position())
                .map_err(|_| CoreError::new(ErrorCode::ResourceLimit, "XML offset overflow"))?;
            let event = reader.read_event_into(&mut event_buf).map_err(|error| {
                CoreError::new(ErrorCode::InvalidXml, format!("malformed XML: {error}"))
            })?;
            let after = usize::try_from(reader.buffer_position())
                .map_err(|_| CoreError::new(ErrorCode::ResourceLimit, "XML offset overflow"))?;
            let is_empty_event = matches!(&event, Event::Empty(_));

            match event {
                Event::Start(_) | Event::Empty(_) => {
                    if elements.len() >= limits.max_elements {
                        return Err(CoreError::new(
                            ErrorCode::ResourceLimit,
                            "XML element limit exceeded",
                        ));
                    }
                    if stack.len() >= limits.max_depth {
                        return Err(CoreError::new(
                            ErrorCode::ResourceLimit,
                            "XML nesting limit exceeded",
                        ));
                    }
                    let is_empty = is_empty_event;
                    let (name, attributes) =
                        parse_start_tag(&bytes, before..after, limits.max_attributes_per_element)?;
                    let id = elements.len();
                    let parent = stack.last().copied();
                    elements.push(Element {
                        name,
                        parent,
                        children: Vec::new(),
                        attributes,
                        open_range: before..after,
                        close_range: if is_empty { after..after } else { 0..0 },
                        full_range: if is_empty { before..after } else { before..0 },
                        empty: is_empty,
                    });
                    if let Some(parent) = parent {
                        elements[parent].children.push(id);
                    } else {
                        roots.push(id);
                    }
                    if !is_empty {
                        stack.push(id);
                    }
                }
                Event::End(_) => {
                    let id = stack.pop().ok_or_else(|| {
                        CoreError::new(ErrorCode::InvalidXml, "unexpected closing tag")
                    })?;
                    elements[id].close_range = before..after;
                    elements[id].full_range.end = after;
                }
                Event::DocType(_) => {
                    return Err(CoreError::new(
                        ErrorCode::InvalidXml,
                        "DTD declarations are not allowed",
                    ));
                }
                Event::Text(_) | Event::CData(_) if stack.is_empty() => {
                    let outside = std::str::from_utf8(&bytes[before..after]).map_err(|_| {
                        CoreError::new(ErrorCode::InvalidXml, "invalid UTF-8 outside root element")
                    })?;
                    if !outside.trim().is_empty() {
                        return Err(CoreError::new(
                            ErrorCode::InvalidXml,
                            "non-whitespace content outside document element",
                        ));
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            event_buf.clear();
        }

        if !stack.is_empty() {
            return Err(CoreError::new(
                ErrorCode::InvalidXml,
                "unclosed XML element",
            ));
        }
        if roots.len() != 1 {
            return Err(CoreError::ambiguous(format!(
                "expected one document element, found {}",
                roots.len()
            )));
        }

        let mut identities = HashMap::new();
        let mut references: Vec<(String, ElementId)> = Vec::new();
        for (id, element) in elements.iter().enumerate() {
            let identity = element.attributes.iter().find(|attr| attr.name == "z");
            let reference = element.attributes.iter().find(|attr| attr.name == "ref");
            if identity.is_some() && reference.is_some() {
                return Err(CoreError::ambiguous(format!(
                    "{} carries both z and ref identity attributes",
                    element.name
                )));
            }
            if let Some(value) = identity {
                if value.value.is_empty() {
                    return Err(CoreError::validation("empty z identity"));
                }
                if identities.insert(value.value.clone(), id).is_some() {
                    return Err(CoreError::ambiguous(format!(
                        "duplicate z identity '{}'",
                        value.value
                    )));
                }
            }
            if let Some(value) = reference {
                if value.value.is_empty() {
                    return Err(CoreError::validation("empty ref identity"));
                }
                references.push((value.value.clone(), id));
            }
        }
        for (reference, _) in references {
            if !identities.contains_key(&reference) {
                return Err(CoreError::ambiguous(format!(
                    "dangling ref identity '{reference}'"
                )));
            }
        }

        Ok(Self {
            bytes,
            elements,
            root: roots[0],
            identities,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn root(&self) -> ElementId {
        self.root
    }

    pub fn element_count(&self) -> usize {
        self.elements.len()
    }

    pub fn name(&self, id: ElementId) -> &str {
        &self.elements[id].name
    }

    pub fn parent(&self, id: ElementId) -> Option<ElementId> {
        self.elements[id].parent
    }

    pub fn children(&self, id: ElementId) -> &[ElementId] {
        &self.elements[id].children
    }

    pub fn direct_children_named(&self, id: ElementId, name: &str) -> Vec<ElementId> {
        self.children(id)
            .iter()
            .copied()
            .filter(|child| self.name(*child) == name)
            .collect()
    }

    pub fn unique_direct_child(&self, id: ElementId, name: &str) -> Result<ElementId> {
        let candidates = self.direct_children_named(id, name);
        if candidates.len() != 1 {
            return Err(CoreError::ambiguous(format!(
                "expected one {name} child below {}, found {}",
                self.name(id),
                candidates.len()
            )));
        }
        Ok(candidates[0])
    }

    pub fn optional_unique_direct_child(
        &self,
        id: ElementId,
        name: &str,
    ) -> Result<Option<ElementId>> {
        let candidates = self.direct_children_named(id, name);
        if candidates.len() > 1 {
            return Err(CoreError::ambiguous(format!(
                "expected at most one {name} child below {}, found {}",
                self.name(id),
                candidates.len()
            )));
        }
        Ok(candidates.first().copied())
    }

    pub fn descendants_named(&self, id: ElementId, name: &str) -> Vec<ElementId> {
        let mut result = Vec::new();
        let mut pending: Vec<ElementId> = self.children(id).iter().rev().copied().collect();
        while let Some(candidate) = pending.pop() {
            if self.name(candidate) == name {
                result.push(candidate);
            }
            pending.extend(self.children(candidate).iter().rev().copied());
        }
        result
    }

    pub fn attribute(&self, id: ElementId, name: &str) -> Option<&str> {
        self.elements[id]
            .attributes
            .iter()
            .find(|attribute| attribute.name == name)
            .map(|attribute| attribute.value.as_str())
    }

    pub fn require_attribute(&self, id: ElementId, name: &str) -> Result<&str> {
        self.attribute(id, name).ok_or_else(|| {
            CoreError::ambiguous(format!("{} is missing attribute {name}", self.name(id)))
        })
    }

    pub fn resolve(&self, id: ElementId) -> Result<ElementId> {
        match self.attribute(id, "ref") {
            Some(reference) => {
                self.identities.get(reference).copied().ok_or_else(|| {
                    CoreError::ambiguous(format!("dangling reference '{reference}'"))
                })
            }
            None => Ok(id),
        }
    }

    pub fn identity(&self, identity: &str) -> Option<ElementId> {
        self.identities.get(identity).copied()
    }

    /// Returns the first decimal identity above every identity in this
    /// document. RC8 uses `XStream`'s ID-reference mode and its integer sequence
    /// generator; additions deliberately fail closed if a document does not
    /// have that canonical identity shape.
    pub(crate) fn next_numeric_identity(&self) -> Result<u64> {
        let mut maximum = 0_u64;
        for identity in self.identities.keys() {
            let value = identity.parse::<u64>().map_err(|_| {
                CoreError::ambiguous(
                    "campaign contains a non-decimal z identity; fresh identity allocation is unavailable",
                )
            })?;
            if value == 0 || value.to_string() != *identity {
                return Err(CoreError::ambiguous(
                    "campaign contains a noncanonical z identity; fresh identity allocation is unavailable",
                ));
            }
            maximum = maximum.max(value);
        }
        maximum.checked_add(1).ok_or_else(|| {
            CoreError::new(
                ErrorCode::ResourceLimit,
                "campaign z identity sequence is exhausted",
            )
        })
    }

    pub(crate) fn reference_count(&self, identity: &str) -> usize {
        self.elements
            .iter()
            .filter(|element| {
                element
                    .attributes
                    .iter()
                    .any(|attribute| attribute.name == "ref" && attribute.value == identity)
            })
            .count()
    }

    pub fn simple_text(&self, id: ElementId) -> Result<String> {
        if !self.children(id).is_empty() {
            return Err(CoreError::ambiguous(format!(
                "{} is not a scalar element",
                self.name(id)
            )));
        }
        let range = self.inner_range(id)?;
        let raw = &self.bytes[range];
        if raw.contains(&b'<') {
            return Err(CoreError::new(
                ErrorCode::InvalidXml,
                format!("{} contains non-text markup", self.name(id)),
            ));
        }
        decode_xml(raw)
    }

    pub fn child_text(&self, id: ElementId, name: &str) -> Result<String> {
        let child = self.unique_direct_child(id, name)?;
        self.simple_text(child)
    }

    pub fn inner_range(&self, id: ElementId) -> Result<Range<usize>> {
        let element = &self.elements[id];
        if element.empty {
            return Err(CoreError::validation(format!(
                "{} is self-closing and has no patchable inner span",
                element.name
            )));
        }
        Ok(element.open_range.end..element.close_range.start)
    }

    pub fn full_range(&self, id: ElementId) -> Range<usize> {
        self.elements[id].full_range.clone()
    }

    pub fn text_patch(
        &self,
        id: ElementId,
        value: &str,
        label: impl Into<String>,
    ) -> Result<SpanPatch> {
        if !self.children(id).is_empty() {
            return Err(CoreError::validation(format!(
                "refusing to replace structured {} content",
                self.name(id)
            )));
        }
        let range = self.inner_range(id)?;
        Ok(SpanPatch::new(
            range.clone(),
            self.bytes[range].to_vec(),
            escape_xml_text(value).into_bytes(),
            label,
        ))
    }

    pub fn raw_inner_patch(
        &self,
        id: ElementId,
        replacement: impl Into<Vec<u8>>,
        label: impl Into<String>,
    ) -> Result<SpanPatch> {
        let range = self.inner_range(id)?;
        Ok(SpanPatch::new(
            range.clone(),
            self.bytes[range].to_vec(),
            replacement,
            label,
        ))
    }

    /// Inserts bytes immediately before an element's checked closing tag.
    /// The closing tag itself is retained in the replacement so stale patch
    /// detection is guarded by observed source bytes rather than an empty
    /// insertion span.
    pub(crate) fn prepend_to_closing_tag_patch(
        &self,
        id: ElementId,
        insertion: impl AsRef<[u8]>,
        label: impl Into<String>,
    ) -> Result<SpanPatch> {
        let element = &self.elements[id];
        if element.empty {
            return Err(CoreError::validation(format!(
                "{} is self-closing and cannot accept child insertion",
                element.name
            )));
        }
        let range = element.close_range.clone();
        let expected = self.bytes[range.clone()].to_vec();
        let mut replacement = Vec::with_capacity(insertion.as_ref().len() + expected.len());
        replacement.extend_from_slice(insertion.as_ref());
        replacement.extend_from_slice(&expected);
        Ok(SpanPatch::new(range, expected, replacement, label))
    }

    pub fn attribute_patch(
        &self,
        id: ElementId,
        name: &str,
        value: &str,
        label: impl Into<String>,
    ) -> Result<SpanPatch> {
        let attribute = self.elements[id]
            .attributes
            .iter()
            .find(|attribute| attribute.name == name)
            .ok_or_else(|| {
                CoreError::ambiguous(format!("{} is missing attribute {name}", self.name(id)))
            })?;
        Ok(SpanPatch::new(
            attribute.value_range.clone(),
            self.bytes[attribute.value_range.clone()].to_vec(),
            escape_xml_attribute(value).into_bytes(),
            label,
        ))
    }

    pub fn raw_bytes(&self, range: Range<usize>) -> &[u8] {
        &self.bytes[range]
    }
}

fn parse_start_tag(
    document: &[u8],
    range: Range<usize>,
    max_attributes: usize,
) -> Result<(String, Vec<Attribute>)> {
    let raw = &document[range.clone()];
    if raw.first() != Some(&b'<') || raw.last() != Some(&b'>') {
        return Err(CoreError::new(
            ErrorCode::InvalidXml,
            "invalid start-tag span",
        ));
    }
    let mut index = 1usize;
    skip_ascii_whitespace(raw, &mut index);
    let name_start = index;
    while index < raw.len()
        && !raw[index].is_ascii_whitespace()
        && !matches!(raw[index], b'/' | b'>')
    {
        index += 1;
    }
    if name_start == index {
        return Err(CoreError::new(ErrorCode::InvalidXml, "empty element name"));
    }
    let name = std::str::from_utf8(&raw[name_start..index])
        .map_err(|_| CoreError::new(ErrorCode::InvalidXml, "invalid element name"))?
        .to_owned();
    let mut attributes = Vec::new();
    let mut names = HashSet::new();

    loop {
        skip_ascii_whitespace(raw, &mut index);
        if index >= raw.len() || raw[index] == b'>' || raw[index] == b'/' {
            break;
        }
        if attributes.len() >= max_attributes {
            return Err(CoreError::new(
                ErrorCode::ResourceLimit,
                format!("attribute limit exceeded on {name}"),
            ));
        }
        let attr_start = index;
        while index < raw.len()
            && !raw[index].is_ascii_whitespace()
            && !matches!(raw[index], b'=' | b'/' | b'>')
        {
            index += 1;
        }
        if attr_start == index {
            return Err(CoreError::new(
                ErrorCode::InvalidXml,
                "empty attribute name",
            ));
        }
        let attr_name = std::str::from_utf8(&raw[attr_start..index])
            .map_err(|_| CoreError::new(ErrorCode::InvalidXml, "invalid attribute name"))?
            .to_owned();
        if !names.insert(attr_name.clone()) {
            return Err(CoreError::new(
                ErrorCode::InvalidXml,
                format!("duplicate attribute {attr_name}"),
            ));
        }
        skip_ascii_whitespace(raw, &mut index);
        if raw.get(index) != Some(&b'=') {
            return Err(CoreError::new(
                ErrorCode::InvalidXml,
                format!("attribute {attr_name} is missing '='"),
            ));
        }
        index += 1;
        skip_ascii_whitespace(raw, &mut index);
        let quote = *raw
            .get(index)
            .ok_or_else(|| CoreError::new(ErrorCode::InvalidXml, "unterminated attribute"))?;
        if !matches!(quote, b'\'' | b'"') {
            return Err(CoreError::new(
                ErrorCode::InvalidXml,
                format!("attribute {attr_name} is not quoted"),
            ));
        }
        index += 1;
        let value_start = index;
        while index < raw.len() && raw[index] != quote {
            index += 1;
        }
        if index >= raw.len() {
            return Err(CoreError::new(
                ErrorCode::InvalidXml,
                "unterminated attribute",
            ));
        }
        let value_end = index;
        let value = decode_xml(&raw[value_start..value_end])?;
        attributes.push(Attribute {
            name: attr_name,
            value,
            value_range: (range.start + value_start)..(range.start + value_end),
        });
        index += 1;
    }
    Ok((name, attributes))
}

fn skip_ascii_whitespace(bytes: &[u8], index: &mut usize) {
    while *index < bytes.len() && bytes[*index].is_ascii_whitespace() {
        *index += 1;
    }
}

fn decode_xml(raw: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| CoreError::new(ErrorCode::InvalidXml, "invalid UTF-8 text"))?;
    if !text.contains('&') {
        return Ok(text.to_owned());
    }
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(relative) = text[cursor..].find('&') {
        let start = cursor + relative;
        output.push_str(&text[cursor..start]);
        let end = text[start + 1..]
            .find(';')
            .map(|relative| start + 1 + relative)
            .ok_or_else(|| {
                CoreError::new(ErrorCode::InvalidXml, "unterminated entity reference")
            })?;
        let entity = &text[start + 1..end];
        match entity {
            "amp" => output.push('&'),
            "lt" => output.push('<'),
            "gt" => output.push('>'),
            "apos" => output.push('\''),
            "quot" => output.push('"'),
            _ if entity.starts_with("#x") => {
                let value = u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| {
                        CoreError::new(ErrorCode::InvalidXml, "invalid numeric entity")
                    })?;
                output.push(value);
            }
            _ if entity.starts_with('#') => {
                let value = entity[1..]
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| {
                        CoreError::new(ErrorCode::InvalidXml, "invalid numeric entity")
                    })?;
                output.push(value);
            }
            _ => {
                return Err(CoreError::new(
                    ErrorCode::InvalidXml,
                    format!("unsupported entity '&{entity};'"),
                ));
            }
        }
        cursor = end + 1;
    }
    output.push_str(&text[cursor..]);
    Ok(output)
}

pub(crate) fn escape_xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn escape_xml_attribute(value: &str) -> String {
    escape_xml_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn ascii_case_insensitive_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::apply_patches;

    #[test]
    fn indexes_references_and_exact_spans() {
        let bytes = br#"<?xml version="1.0"?><root z="1"><name f="A&amp;B">Old &lt;name&gt;</name><link ref="2"></link><thing z="2"/></root>"#.to_vec();
        let doc = XmlDocument::parse(bytes.clone(), XmlLimits::default()).unwrap();
        let name = doc.unique_direct_child(doc.root(), "name").unwrap();
        assert_eq!(doc.simple_text(name).unwrap(), "Old <name>");
        assert_eq!(doc.attribute(name, "f"), Some("A&B"));
        let patch = doc.text_patch(name, "New & safe", "name").unwrap();
        let output = apply_patches(&bytes, &[patch]).unwrap();
        assert!(std::str::from_utf8(&output)
            .unwrap()
            .contains("New &amp; safe"));
        XmlDocument::parse(output, XmlLimits::default()).unwrap();
    }

    #[test]
    fn duplicate_dangling_and_dtd_fail() {
        for xml in [
            "<r z=\"1\"><a z=\"2\"/><b z=\"2\"/></r>",
            "<r z=\"1\"><a ref=\"9\"/></r>",
            "<!DOCTYPE r [<!ENTITY x \"boom\">]><r z=\"1\">&x;</r>",
        ] {
            assert!(XmlDocument::parse(xml.as_bytes().to_vec(), XmlLimits::default()).is_err());
        }
    }

    #[test]
    fn fresh_identity_allocation_requires_canonical_decimal_ids() {
        let canonical = XmlDocument::parse(
            br#"<r z="1"><a z="3"></a></r>"#.to_vec(),
            XmlLimits::default(),
        )
        .unwrap();
        assert_eq!(canonical.next_numeric_identity().unwrap(), 4);

        for xml in [
            r#"<r z="0"></r>"#,
            r#"<r z="01"></r>"#,
            r#"<r z="mod-id"></r>"#,
        ] {
            let document =
                XmlDocument::parse(xml.as_bytes().to_vec(), XmlLimits::default()).unwrap();
            assert_eq!(
                document.next_numeric_identity().unwrap_err().code,
                ErrorCode::AmbiguousStructure
            );
        }
    }
}
