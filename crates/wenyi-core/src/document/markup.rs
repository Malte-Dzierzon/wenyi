//! Minimal HTML/XHTML DOM for EPUB markup processing.
//!
//! Lenient parser + serializer tuned for EPUB XHTML. Preserves attribute
//! order, comments, and processing instructions; normalizes tag names to
//! lowercase (matching BeautifulSoup `html.parser` behavior closely enough
//! for the golden fixtures).

/// Node identifier (index into the arena).
pub type NodeId = usize;

/// DOM node: element, text, comment, or processing instruction.
#[derive(Debug, Clone)]
pub enum Node {
    /// Document root (not serialized).
    Root {
        /// Child node ids in order.
        children: Vec<NodeId>,
    },
    /// Element with tag, attributes, children.
    Element {
        /// Lowercase tag name.
        tag: String,
        /// Attributes in document order.
        attrs: Vec<(String, String)>,
        /// Child node ids in order.
        children: Vec<NodeId>,
    },
    /// Text node.
    Text {
        /// Raw text.
        text: String,
    },
    /// HTML comment (`<!--...-->`).
    Comment {
        /// Comment body.
        body: String,
    },
    /// Processing instruction (`<?...?>`).
    Pi {
        /// PI body.
        body: String,
    },
    /// Doctype declaration.
    Doctype {
        /// Doctype body.
        body: String,
    },
}

/// Arena DOM with parent pointers.
#[derive(Debug, Clone, Default)]
pub struct Dom {
    /// All nodes; id 0 is always the root.
    pub nodes: Vec<Node>,
    /// Parent id per node (root parents to itself).
    pub parents: Vec<NodeId>,
}

impl Dom {
    /// Empty document with a root node.
    pub fn new() -> Self {
        Self {
            nodes: vec![Node::Root {
                children: Vec::new(),
            }],
            parents: vec![0],
        }
    }

    /// Append a node as a child of `parent`; returns its id.
    pub fn append(&mut self, parent: NodeId, node: Node) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        self.parents.push(parent);
        match &mut self.nodes[parent] {
            Node::Root { children } | Node::Element { children, .. } => children.push(id),
            _ => {}
        }
        id
    }

    /// Children of a node in order.
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        match &self.nodes[id] {
            Node::Root { children } | Node::Element { children, .. } => children.clone(),
            _ => Vec::new(),
        }
    }

    /// Parent of a node (root parents to itself).
    pub fn parent(&self, id: NodeId) -> NodeId {
        self.parents[id]
    }

    /// Ancestors from innermost to root (excluding self and root).
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cursor = self.parents[id];
        while cursor != 0 {
            out.push(cursor);
            cursor = self.parents[cursor];
        }
        out
    }

    /// All descendants in document order.
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.children(id).into_iter().rev().collect();
        while let Some(next) = stack.pop() {
            out.push(next);
            for child in self.children(next).into_iter().rev() {
                stack.push(child);
            }
        }
        out
    }

    /// Previous siblings (nearest first).
    pub fn previous_siblings(&self, id: NodeId) -> Vec<NodeId> {
        let parent = self.parents[id];
        let siblings = self.children(parent);
        let pos = siblings.iter().position(|s| *s == id).unwrap_or(0);
        siblings[..pos].iter().rev().copied().collect()
    }

    /// Next siblings in order.
    pub fn next_siblings(&self, id: NodeId) -> Vec<NodeId> {
        let parent = self.parents[id];
        let siblings = self.children(parent);
        let pos = siblings
            .iter()
            .position(|s| *s == id)
            .unwrap_or(siblings.len());
        siblings[pos + 1..].to_vec()
    }

    /// Tag name for elements.
    pub fn tag(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id] {
            Node::Element { tag, .. } => Some(tag),
            _ => None,
        }
    }

    /// Attribute value for elements.
    pub fn attr(&self, id: NodeId, key: &str) -> Option<&str> {
        match &self.nodes[id] {
            Node::Element { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    /// Set (or add) an attribute, preserving order.
    pub fn set_attr(&mut self, id: NodeId, key: &str, value: &str) {
        if let Node::Element { attrs, .. } = &mut self.nodes[id] {
            if let Some(slot) = attrs.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value.to_string();
            } else {
                attrs.push((key.to_string(), value.to_string()));
            }
        }
    }

    /// Remove an attribute, returning its value.
    pub fn remove_attr(&mut self, id: NodeId, key: &str) -> Option<String> {
        if let Node::Element { attrs, .. } = &mut self.nodes[id] {
            if let Some(pos) = attrs.iter().position(|(k, _)| k == key) {
                return Some(attrs.remove(pos).1);
            }
        }
        None
    }

    /// Whether the node is an element with the attribute.
    pub fn has_attr(&self, id: NodeId, key: &str) -> bool {
        self.attr(id, key).is_some()
    }

    /// Concatenated text of a node and descendants (oracle `get_text`).
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id] {
            Node::Text { text } => out.push_str(text),
            Node::Root { children } | Node::Element { children, .. } => {
                for child in children {
                    self.collect_text(*child, out);
                }
            }
            _ => {}
        }
    }

    /// Text with separator and optional stripping (oracle `get_text(sep, strip)`).
    pub fn get_text(&self, id: NodeId, separator: &str, strip: bool) -> String {
        let mut parts = Vec::new();
        self.collect_text_parts(id, &mut parts);
        let mut joined = parts.join(separator);
        if strip {
            joined = joined
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(separator);
            // Oracle strip=True strips each piece then joins; emulate closely.
            joined = joined.trim().to_string();
        }
        joined
    }

    fn collect_text_parts(&self, id: NodeId, out: &mut Vec<String>) {
        match &self.nodes[id] {
            Node::Text { text } => out.push(text.clone()),
            Node::Root { children } | Node::Element { children, .. } => {
                for child in children {
                    self.collect_text_parts(*child, out);
                }
            }
            _ => {}
        }
    }

    /// Find the first descendant (document order) matching a predicate.
    pub fn find_descendant(
        &self,
        id: NodeId,
        mut pred: impl FnMut(NodeId) -> bool,
    ) -> Option<NodeId> {
        self.descendants(id).into_iter().find(|d| pred(*d))
    }

    /// All descendants matching a predicate (document order).
    pub fn find_all_descendants(
        &self,
        id: NodeId,
        mut pred: impl FnMut(NodeId) -> bool,
    ) -> Vec<NodeId> {
        self.descendants(id)
            .into_iter()
            .filter(|d| pred(*d))
            .collect()
    }

    /// Nearest ancestor (excluding self) matching a predicate.
    pub fn find_parent(&self, id: NodeId, mut pred: impl FnMut(NodeId) -> bool) -> Option<NodeId> {
        self.ancestors(id).into_iter().find(|a| pred(*a))
    }

    /// Next node in document order with an attribute.
    pub fn find_next_with_attr(&self, id: NodeId, key: &str) -> Option<NodeId> {
        // Document order from root; take nodes after `id`.
        let order = self.descendants(0);
        let pos = order.iter().position(|o| *o == id)?;
        order[pos + 1..]
            .iter()
            .find(|o| self.has_attr(**o, key))
            .copied()
    }

    /// Detach a node from its parent (children travel with it).
    pub fn detach(&mut self, id: NodeId) {
        let parent = self.parents[id];
        match &mut self.nodes[parent] {
            Node::Root { children } | Node::Element { children, .. } => {
                children.retain(|c| *c != id);
            }
            _ => {}
        }
    }

    /// Insert `node` before `sibling` under the same parent.
    pub fn insert_before(&mut self, sibling: NodeId, node: Node) -> NodeId {
        let parent = self.parents[sibling];
        let id = self.nodes.len();
        self.nodes.push(node);
        self.parents.push(parent);
        match &mut self.nodes[parent] {
            Node::Root { children } | Node::Element { children, .. } => {
                if let Some(pos) = children.iter().position(|c| *c == sibling) {
                    children.insert(pos, id);
                } else {
                    children.push(id);
                }
            }
            _ => {}
        }
        id
    }

    /// Move an existing node to become the last child of `parent`.
    pub fn move_to(&mut self, id: NodeId, parent: NodeId) {
        self.detach(id);
        self.parents[id] = parent;
        match &mut self.nodes[parent] {
            Node::Root { children } | Node::Element { children, .. } => children.push(id),
            _ => {}
        }
    }

    /// Insert `node` after `sibling` under the same parent.
    pub fn insert_after(&mut self, sibling: NodeId, node: Node) -> NodeId {
        let parent = self.parents[sibling];
        let id = self.nodes.len();
        self.nodes.push(node);
        self.parents.push(parent);
        match &mut self.nodes[parent] {
            Node::Root { children } | Node::Element { children, .. } => {
                if let Some(pos) = children.iter().position(|c| *c == sibling) {
                    children.insert(pos + 1, id);
                } else {
                    children.push(id);
                }
            }
            _ => {}
        }
        id
    }

    /// Move an existing node to directly before `sibling`.
    pub fn move_before(&mut self, id: NodeId, sibling: NodeId) {
        self.detach(id);
        let parent = self.parents[sibling];
        self.parents[id] = parent;
        match &mut self.nodes[parent] {
            Node::Root { children } | Node::Element { children, .. } => {
                if let Some(pos) = children.iter().position(|c| *c == sibling) {
                    children.insert(pos, id);
                } else {
                    children.push(id);
                }
            }
            _ => {}
        }
    }

    /// Move an existing node to directly after `sibling`.
    pub fn move_after(&mut self, id: NodeId, sibling: NodeId) {
        self.detach(id);
        let parent = self.parents[sibling];
        self.parents[id] = parent;
        match &mut self.nodes[parent] {
            Node::Root { children } | Node::Element { children, .. } => {
                if let Some(pos) = children.iter().position(|c| *c == sibling) {
                    children.insert(pos + 1, id);
                } else {
                    children.push(id);
                }
            }
            _ => {}
        }
    }

    /// Remove all children of an element (oracle `Tag.clear`).
    pub fn clear(&mut self, id: NodeId) {
        match &mut self.nodes[id] {
            Node::Root { children } | Node::Element { children, .. } => children.clear(),
            _ => {}
        }
    }

    /// Replace an element with its children (oracle `Tag.unwrap`).
    pub fn unwrap(&mut self, id: NodeId) {
        let parent = self.parents[id];
        let children = self.children(id);
        match &mut self.nodes[parent] {
            Node::Root { children: siblings }
            | Node::Element {
                children: siblings, ..
            } => {
                if let Some(pos) = siblings.iter().position(|c| *c == id) {
                    siblings.splice(pos..pos + 1, children.iter().copied());
                }
            }
            _ => {}
        }
        for child in children {
            self.parents[child] = parent;
        }
        if let Node::Element { children, .. } = &mut self.nodes[id] {
            children.clear();
        }
    }

    /// Remove a node and its subtree (oracle `Tag.decompose`).
    pub fn remove_subtree(&mut self, id: NodeId) {
        self.detach(id);
    }

    /// First direct element child with the given tag.
    pub fn direct_child(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        self.children(id)
            .into_iter()
            .find(|c| self.tag(*c) == Some(tag))
    }

    /// Serialize one node (element or text) and its subtree.
    pub fn serialize_node_str(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.serialize_node(id, &mut out);
        out
    }

    /// Serialize the inner content of a node (oracle `decode_contents`).
    pub fn serialize_contents(&self, id: NodeId) -> String {
        let mut out = String::new();
        for child in self.children(id) {
            self.serialize_node(child, &mut out);
        }
        out
    }

    /// Whether the node is a text node (excludes comments/PIs).
    pub fn is_text(&self, id: NodeId) -> bool {
        matches!(self.nodes[id], Node::Text { .. })
    }

    /// Whether the node is a processing instruction.
    pub fn is_pi(&self, id: NodeId) -> bool {
        matches!(self.nodes[id], Node::Pi { .. })
    }

    /// Serialize the document (children of root concatenated).
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for child in self.children(0) {
            self.serialize_node(child, &mut out);
        }
        out
    }

    fn serialize_node(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id] {
            Node::Root { children } => {
                for child in children {
                    self.serialize_node(*child, out);
                }
            }
            Node::Element {
                tag,
                attrs,
                children,
            } => {
                out.push('<');
                out.push_str(tag);
                for (key, value) in attrs {
                    out.push(' ');
                    out.push_str(key);
                    out.push_str("=\"");
                    out.push_str(&escape_attr(value));
                    out.push('"');
                }
                if children.is_empty() && is_void(tag) {
                    out.push_str("/>");
                    return;
                }
                out.push('>');
                for child in children {
                    self.serialize_node(*child, out);
                }
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
            Node::Text { text } => out.push_str(&escape_text(text)),
            Node::Comment { body } => {
                out.push_str("<!--");
                out.push_str(body);
                out.push_str("-->");
            }
            Node::Pi { body } => {
                out.push_str("<?");
                out.push_str(body);
                out.push('>');
            }
            Node::Doctype { body } => {
                out.push_str("<!DOCTYPE ");
                out.push_str(body);
                out.push('>');
            }
        }
    }
}

fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(value: &str) -> String {
    escape_text(value).replace('"', "&quot;")
}

/// Parse lenient HTML/XHTML into a `Dom`.
pub fn parse_html(text: &str) -> Dom {
    let mut dom = Dom::new();
    let mut stack: Vec<NodeId> = vec![0];
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if text[i..].starts_with("<!--") {
                let end = text[i..]
                    .find("-->")
                    .map(|p| i + p + 3)
                    .unwrap_or(bytes.len());
                let body = text[i + 4..end.saturating_sub(3)].to_string();
                let parent = *stack.last().unwrap();
                dom.append(parent, Node::Comment { body });
                i = end;
                continue;
            }
            if text[i..].starts_with("<?") {
                let end = text[i..]
                    .find('>')
                    .map(|p| i + p + 1)
                    .unwrap_or(bytes.len());
                let body = text[i + 2..end.saturating_sub(1)].to_string();
                let parent = *stack.last().unwrap();
                dom.append(parent, Node::Pi { body });
                i = end;
                continue;
            }
            if text[i..].to_uppercase().starts_with("<!DOCTYPE") {
                let end = text[i..]
                    .find('>')
                    .map(|p| i + p + 1)
                    .unwrap_or(bytes.len());
                let body = text[i + 9..end.saturating_sub(1)].trim().to_string();
                let parent = *stack.last().unwrap();
                dom.append(parent, Node::Doctype { body });
                i = end;
                continue;
            }
            if let Some(end) = text[i..].find('>') {
                let tag_text = &text[i + 1..i + end];
                if let Some(close_name) = tag_text.strip_prefix('/') {
                    let close_name = close_name.trim().to_lowercase();
                    while stack.len() > 1 {
                        let top = *stack.last().unwrap();
                        stack.pop();
                        if dom.tag(top) == Some(close_name.as_str()) {
                            break;
                        }
                    }
                } else if !tag_text.starts_with(['!', '?']) {
                    let trimmed = tag_text.trim_end();
                    let self_closing = trimmed.ends_with('/');
                    let name_end = trimmed
                        .find(|c: char| c.is_whitespace() || c == '/')
                        .unwrap_or(trimmed.len());
                    let tag = trimmed[..name_end].to_lowercase();
                    if !tag.is_empty() {
                        let rest = trimmed[name_end..].trim_end_matches('/').trim();
                        let attrs = parse_attrs(rest);
                        let parent = *stack.last().unwrap();
                        let id = dom.append(
                            parent,
                            Node::Element {
                                tag: tag.clone(),
                                attrs,
                                children: Vec::new(),
                            },
                        );
                        if !self_closing && !is_void(&tag) {
                            stack.push(id);
                        }
                    }
                }
                i += end + 1;
            } else {
                break;
            }
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b'<' {
                i += 1;
            }
            let decoded = decode_entities(&text[start..i]);
            let parent = *stack.last().unwrap();
            dom.append(parent, Node::Text { text: decoded });
        }
    }
    dom
}

fn parse_attrs(rest: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
            && bytes[i] != b'>'
        {
            i += 1;
        }
        let key = rest[start..i].to_lowercase();
        if key.is_empty() || key == "/" {
            i += 1;
            continue;
        }
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                value = decode_entities(&rest[start..i]);
                i += 1;
            } else {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = decode_entities(&rest[start..i]);
            }
        }
        attrs.push((key, value));
    }
    attrs
}

/// Decode the common HTML entities (oracle parity for fixture markup).
fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", "\u{a0}")
}
