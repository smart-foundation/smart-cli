//! Runtime contract introspection: the schema's own node tree, decoded and
//! walked, so the CLI can address a schema this build never compiled.
//!
//! # Why
//!
//! The typed path uses the generated bindings. The **generic path** is for a
//! schema the CLI has not compiled — a service that ships a contract this
//! build has never seen. The service publishes the schema's node tree (the
//! contract's own `CodeGeneratorRequest`: the same bytes `capnp compile`
//! produces), and the CLI walks it to learn
//!
//! - the interface's **type id** — the node's id, the very number the
//!   generated code would have assigned;
//! - the method's **id** — the method node's id, the declared ordinal;
//! - the params and results **shapes** — the struct nodes' fields.
//!
//! With those three, the CLI builds the params, issues the call and reads the
//! results WITHOUT a single compiled binding: the contract is the only
//! vocabulary, so a service the CLI has never heard of still works.
//!
//! # The layout
//!
//! capnp's struct layout is fixed by the schema: a data section and a pointer
//! section, with every field at the offset the schema declares. The node tree
//! carries those offsets, and capnp's own layout primitives read and write
//! them — the same primitives the generated code uses.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use capnp::private::layout;
use capnp::schema_capnp::{code_generator_request, field, node, type_ as schema_type};

/// The metadata key a service publishes a schema's node tree under.
///
/// The contract's `CommandRegistration.metadata` is an open string map — the
/// one place the contract leaves room for a client to learn something the
/// schema does not carry. A service that serves a schema a client has never
/// compiled publishes that schema's node tree here, and the client addresses
/// the method from it.
pub const SCHEMA_METADATA_KEY: &str = "capnpSchema";

/// A field's type, as the node tree declares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldType {
    /// No value.
    Void,
    /// A boolean.
    Bool,
    /// A signed 8/16/32/64-bit integer.
    Int(u8),
    /// An unsigned 8/16/32/64-bit integer.
    UInt(u8),
    /// A 32/64-bit float.
    Float(u8),
    /// Text.
    Text,
    /// Opaque bytes.
    Data,
    /// An enum (by type id).
    Enum(u64),
    /// A struct (by type id).
    Struct(u64),
    /// An interface (by type id).
    Interface(u64),
    /// A list of another type.
    List(Box<FieldType>),
    /// A pointer the schema does not constrain.
    AnyPointer,
}

/// One field of a struct, as the node tree declares it.
#[derive(Clone, Debug)]
pub struct FieldInfo {
    /// The field's name (the schema's own spelling).
    pub name: String,
    /// The field's offset in its section.
    pub offset: u32,
    /// The field's type.
    pub ty: FieldType,
}

/// One struct of the schema, by type id.
#[derive(Clone, Debug)]
pub struct StructInfo {
    /// The struct's type id.
    pub type_id: u64,
    /// The struct's data-section size, in words.
    pub data_words: u16,
    /// The struct's pointer-section size.
    pub pointer_count: u16,
    /// The struct's fields, in schema order.
    pub fields: Vec<FieldInfo>,
}

/// One method of an interface.
#[derive(Clone, Debug)]
pub struct MethodInfo {
    /// The method's name (the schema's own spelling).
    pub name: String,
    /// The method's id — the ordinal the schema declares.
    pub id: u16,
    /// The method's node id in the tree (the same number as `id`).
    pub node_id: u64,
    /// The type id of the method's params struct.
    pub params_type_id: u64,
    /// The type id of the method's results struct.
    pub results_type_id: u64,
}

/// One interface of the schema.
#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    /// The interface's name.
    pub name: String,
    /// The interface's type id — what an RPC call carries.
    pub type_id: u64,
    /// The interface's methods.
    pub methods: Vec<MethodInfo>,
}

/// The schema's own node tree, decoded into the shape the CLI walks.
#[derive(Clone, Debug, Default)]
pub struct SchemaTree {
    interfaces: Vec<InterfaceInfo>,
    structs: Vec<StructInfo>,
}

impl SchemaTree {
    /// Decode a `CodeGeneratorRequest` (the bytes `capnp compile -o-`
    /// produces) into the tree.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let reader = capnp::serialize::read_message(
            &mut &bytes[..],
            capnp::message::ReaderOptions::new(),
        )
        .ok()?;
        let request = reader.get_root::<code_generator_request::Reader>().ok()?;
        let mut tree = SchemaTree::default();
        // The method's id lives on the method's NODE (the `Method` struct of
        // the interface does not carry it), so the tree remembers every
        // node's scope and display name to find it.
        let mut scopes: Vec<(u64, String, u64)> = Vec::new();
        for node in request.get_nodes().into_iter().flatten() {
            let node_id = node.get_id();
            scopes.push((
                node.get_scope_id(),
                text(node.get_display_name()),
                node_id,
            ));
            match node.which() {
                Ok(node::Which::Interface(interface)) => {
                    let mut methods = Vec::new();
                    let interface_name = text(node.get_display_name());
                    for method in interface.get_methods().into_iter().flatten() {
                        let name = text(method.get_name());
                        let node_id = scopes
                            .iter()
                            .find(|(scope, display, _)| {
                                *scope == node_id
                                    && (display == &format!("{interface_name}.{name}")
                                        || display.ends_with(&format!(".{name}")))
                            })
                            .map(|(_, _, id)| *id)
                            .unwrap_or_default();
                        methods.push(MethodInfo {
                            name,
                            id: node_id as u16,
                            node_id,
                            params_type_id: method.get_param_struct_type(),
                            results_type_id: method.get_result_struct_type(),
                        });
                    }
                    tree.interfaces.push(InterfaceInfo {
                        name: short_name(&text(node.get_display_name())),
                        type_id: node_id,
                        methods,
                    });
                }
                Ok(node::Which::Struct(struct_)) => {
                    let mut fields = Vec::new();
                    for field in struct_.get_fields().into_iter().flatten() {
                        let Some(slot) = field_slot(field) else {
                            continue;
                        };
                        fields.push(FieldInfo {
                            name: text(field.get_name()),
                            offset: slot.get_offset(),
                            ty: field_type(slot.get_type()),
                        });
                    }
                    tree.structs.push(StructInfo {
                        type_id: node_id,
                        data_words: struct_.get_data_word_count(),
                        pointer_count: struct_.get_pointer_count(),
                        fields,
                    });
                }
                _ => {}
            }
        }
        Some(tree)
    }

    /// The interface the schema declares under `name`.
    pub fn interface(&self, name: &str) -> Option<&InterfaceInfo> {
        self.interfaces.iter().find(|i| i.name == name)
    }

    /// One method of one interface, by name.
    pub fn method(&self, interface: &str, method: &str) -> Option<&MethodInfo> {
        self.interface(interface)?
            .methods
            .iter()
            .find(|m| m.name == method)
    }

    /// One struct, by type id.
    pub fn struct_info(&self, type_id: u64) -> Option<&StructInfo> {
        self.structs.iter().find(|s| s.type_id == type_id)
    }
}

/// The short name of a node: capnp's display names carry the file prefix
/// (`fixture.capnp:Echo`), and the contract names the interface by its own
/// name (`Echo`).
fn short_name(display: &str) -> String {
    match display.rsplit_once(':') {
        Some((_, name)) => name.to_owned(),
        None => display.to_owned(),
    }
}

/// The slot of a field (a group field has no slot).
fn field_slot(field: field::Reader<'_>) -> Option<field::slot::Reader<'_>> {
    match field.which() {
        Ok(field::Which::Slot(slot)) => Some(slot),
        Ok(field::Which::Group(_)) | Err(_) => None,
    }
}

/// A node tree text value, as a Rust string.
fn text(value: Result<capnp::text::Reader<'_>, capnp::Error>) -> String {
    value
        .ok()
        .and_then(|text| text.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

/// The type of a field, as the node tree declares it.
fn field_type(ty: Result<schema_type::Reader<'_>, capnp::Error>) -> FieldType {
    let Ok(ty) = ty else {
        return FieldType::AnyPointer;
    };
    match ty.which() {
        Ok(schema_type::Which::Void(_)) => FieldType::Void,
        Ok(schema_type::Which::Bool(_)) => FieldType::Bool,
        Ok(schema_type::Which::Int8(_)) => FieldType::Int(8),
        Ok(schema_type::Which::Int16(_)) => FieldType::Int(16),
        Ok(schema_type::Which::Int32(_)) => FieldType::Int(32),
        Ok(schema_type::Which::Int64(_)) => FieldType::Int(64),
        Ok(schema_type::Which::Uint8(_)) => FieldType::UInt(8),
        Ok(schema_type::Which::Uint16(_)) => FieldType::UInt(16),
        Ok(schema_type::Which::Uint32(_)) => FieldType::UInt(32),
        Ok(schema_type::Which::Uint64(_)) => FieldType::UInt(64),
        Ok(schema_type::Which::Float32(_)) => FieldType::Float(32),
        Ok(schema_type::Which::Float64(_)) => FieldType::Float(64),
        Ok(schema_type::Which::Text(_)) => FieldType::Text,
        Ok(schema_type::Which::Data(_)) => FieldType::Data,
        Ok(schema_type::Which::List(list)) => {
            FieldType::List(Box::new(field_type(list.get_element_type())))
        }
        Ok(schema_type::Which::Enum(enum_)) => FieldType::Enum(enum_.get_type_id()),
        Ok(schema_type::Which::Struct(struct_)) => FieldType::Struct(struct_.get_type_id()),
        Ok(schema_type::Which::Interface(interface)) => {
            FieldType::Interface(interface.get_type_id())
        }
        Ok(schema_type::Which::AnyPointer(_)) | Err(_) => FieldType::AnyPointer,
    }
}

// ── building a params struct without a compiled binding ────────────────────

/// The layout of the struct being built right now.
///
/// capnp's root builder is created by the message builder, which knows
/// nothing about the schema; the size a struct needs comes from the schema.
/// So the layout waits here for the moment the root builder is created.
#[derive(Clone)]
struct PendingLayout {
    data_words: u16,
    pointer_count: u16,
}

thread_local! {
    static PENDING: RefCell<Option<PendingLayout>> = const { RefCell::new(None) };
}

/// A struct builder whose shape comes from the schema, not from a generated
/// type.
struct DynamicStructBuilder<'a> {
    builder: layout::StructBuilder<'a>,
}

impl<'a> capnp::traits::FromPointerBuilder<'a> for DynamicStructBuilder<'a> {
    fn init_pointer(builder: layout::PointerBuilder<'a>, _len: u32) -> Self {
        let layout = PENDING.with(|slot| slot.borrow().clone());
        let (data, pointers) = match layout {
            Some(layout) => (layout.data_words, layout.pointer_count),
            None => (0, 0),
        };
        Self {
            builder: builder.init_struct(layout::StructSize {
                data,
                pointers,
            }),
        }
    }

    fn get_from_pointer(
        builder: layout::PointerBuilder<'a>,
        _default: Option<&'a [capnp::Word]>,
    ) -> Result<Self, capnp::Error> {
        let layout = PENDING.with(|slot| slot.borrow().clone());
        Ok(Self {
            builder: builder.get_struct(
                layout::StructSize {
                    data: layout.as_ref().map(|l| l.data_words).unwrap_or(0),
                    pointers: layout.as_ref().map(|l| l.pointer_count).unwrap_or(0),
                },
                None,
            )?,
        })
    }
}

/// A field's value, as the user gave it.
#[derive(Clone, Debug)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// An unsigned integer.
    UInt(u64),
    /// A float.
    Float(f64),
    /// Text.
    Text(String),
    /// Opaque bytes.
    Data(Vec<u8>),
}

/// The error a dynamic build reports.
#[derive(Clone, Debug)]
pub struct BuildError {
    /// What went wrong, in plain words.
    pub message: String,
}

impl core::fmt::Display for BuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Build a params struct for a method the CLI never compiled.
///
/// `args` are `field=value` pairs, matched against the struct's field names.
pub fn build_params(
    tree: &SchemaTree,
    method: &MethodInfo,
    args: &[(String, String)],
) -> Result<Vec<u8>, BuildError> {
    let Some(structure) = tree.struct_info(method.params_type_id) else {
        return Err(BuildError {
            message: format!(
                "the schema does not declare the params struct of `{}`",
                method.name
            ),
        });
    };
    let mut message = capnp::message::Builder::new_default();
    PENDING.with(|slot| {
        *slot.borrow_mut() = Some(PendingLayout {
            data_words: structure.data_words,
            pointer_count: structure.pointer_count,
        });
    });
    let mut root: DynamicStructBuilder<'_> = message.init_root();
    PENDING.with(|slot| {
        *slot.borrow_mut() = None;
    });
    for (name, value) in args {
        let Some(field) = structure.fields.iter().find(|f| f.name == *name) else {
            let known: Vec<&str> = structure.fields.iter().map(|f| f.name.as_str()).collect();
            return Err(BuildError {
                message: format!(
                    "the contract's params have no field `{name}` (they have: {})",
                    known.join(", ")
                ),
            });
        };
        set_field(root.builder.reborrow(), field, value)?;
    }
    let bytes = crate::transport::encode_message(&message);
    Ok(bytes)
}

/// Write one field's value into the struct builder.
fn set_field(
    builder: layout::StructBuilder<'_>,
    field: &FieldInfo,
    value: &str,
) -> Result<(), BuildError> {
    let offset = field.offset as usize;
    match &field.ty {
        FieldType::Bool => {
            let parsed = matches!(value, "true" | "1" | "yes" | "on");
            builder.set_bool_field(offset, parsed);
            Ok(())
        }
        FieldType::Int(bits) => {
            let parsed: i64 = value.parse().map_err(|_| BuildError {
                message: format!("`{value}` is not an integer for `{}`", field.name),
            })?;
            match bits {
                8 => builder.set_data_field::<i8>(offset, parsed as i8),
                16 => builder.set_data_field::<i16>(offset, parsed as i16),
                32 => builder.set_data_field::<i32>(offset, parsed as i32),
                _ => builder.set_data_field::<i64>(offset, parsed),
            }
            Ok(())
        }
        FieldType::UInt(bits) => {
            let parsed: u64 = value.parse().map_err(|_| BuildError {
                message: format!("`{value}` is not an unsigned integer for `{}`", field.name),
            })?;
            match bits {
                8 => builder.set_data_field::<u8>(offset, parsed as u8),
                16 => builder.set_data_field::<u16>(offset, parsed as u16),
                32 => builder.set_data_field::<u32>(offset, parsed as u32),
                _ => builder.set_data_field::<u64>(offset, parsed),
            }
            Ok(())
        }
        FieldType::Float(bits) => {
            let parsed: f64 = value.parse().map_err(|_| BuildError {
                message: format!("`{value}` is not a number for `{}`", field.name),
            })?;
            if *bits == 32 {
                builder.set_data_field::<f32>(offset, parsed as f32);
            } else {
                builder.set_data_field::<f64>(offset, parsed);
            }
            Ok(())
        }
        FieldType::Text => {
            let pointer = builder.get_pointer_field(offset);
            let mut text = pointer.init_text(value.len() as u32);
            text.push_str(value);
            Ok(())
        }
        FieldType::Data => {
            let bytes = value.as_bytes();
            let pointer = builder.get_pointer_field(offset);
            let data = pointer.init_data(bytes.len() as u32);
            data.copy_from_slice(bytes);
            Ok(())
        }
        other => Err(BuildError {
            message: format!(
                "the generic path cannot build a `{}` field (`{}`) yet",
                match other {
                    FieldType::List(_) => "list",
                    FieldType::Struct(_) => "struct",
                    FieldType::Enum(_) => "enum",
                    FieldType::Interface(_) => "interface",
                    _ => "pointer",
                },
                field.name
            ),
        }),
    }
}

// ── reading a results struct without a compiled binding ────────────────────

/// One field's value, read from the results.
#[derive(Clone, Debug)]
pub enum ReadValue {
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// An unsigned integer.
    UInt(u64),
    /// A float.
    Float(f64),
    /// Text.
    Text(String),
    /// Opaque bytes.
    Data(Vec<u8>),
    /// A list of values: the count is always known, the items only when the
    /// schema's element type is one the generic path can itemize.
    List {
        /// How many elements the list holds.
        count: u32,
        /// The elements (empty when the element type is not itemizable
        /// without a compiled type).
        items: Vec<ReadValue>,
    },
    /// A nested struct.
    Struct(Vec<(String, ReadValue)>),
    /// A value the schema does not describe (an interface, an unconstrained
    /// pointer).
    Opaque,
}

/// Read a results struct into field-name/value pairs.
pub fn read_results(
    tree: &SchemaTree,
    results_type_id: u64,
    bytes: &[u8],
) -> Option<Vec<(String, ReadValue)>> {
    let structure = tree.struct_info(results_type_id)?;
    let segments = capnp::serialize::read_message(
        &mut &bytes[..],
        capnp::message::ReaderOptions::new(),
    )
    .ok()?
    .into_segments();
    let arena = capnp::private::arena::ReaderArenaImpl::new(
        segments,
        capnp::message::ReaderOptions::new(),
    );
    let root = layout::PointerReader::get_root_from_arena(&arena).ok()?;
    let root = root.get_struct(None).ok()?;
    Some(read_struct(tree, &root, structure))
}

/// Read one struct's fields.
fn read_struct(
    tree: &SchemaTree,
    reader: &layout::StructReader<'_>,
    structure: &StructInfo,
) -> Vec<(String, ReadValue)> {
    let mut out = Vec::new();
    for field in &structure.fields {
        let value = read_field(tree, reader, field);
        out.push((field.name.clone(), value));
    }
    out
}

/// Read one field's value.
fn read_field(
    tree: &SchemaTree,
    reader: &layout::StructReader<'_>,
    field: &FieldInfo,
) -> ReadValue {
    let offset = field.offset as usize;
    match &field.ty {
        FieldType::Bool => ReadValue::Bool(reader.get_bool_field(offset)),
        FieldType::Int(bits) => {
            let value = match bits {
                8 => reader.get_data_field::<i8>(offset) as i64,
                16 => reader.get_data_field::<i16>(offset) as i64,
                32 => reader.get_data_field::<i32>(offset) as i64,
                _ => reader.get_data_field::<i64>(offset),
            };
            ReadValue::Int(value)
        }
        FieldType::UInt(bits) => {
            let value = match bits {
                8 => reader.get_data_field::<u8>(offset) as u64,
                16 => reader.get_data_field::<u16>(offset) as u64,
                32 => reader.get_data_field::<u32>(offset) as u64,
                _ => reader.get_data_field::<u64>(offset),
            };
            ReadValue::UInt(value)
        }
        FieldType::Float(bits) => {
            let value = if *bits == 32 {
                reader.get_data_field::<f32>(offset) as f64
            } else {
                reader.get_data_field::<f64>(offset)
            };
            ReadValue::Float(value)
        }
        FieldType::Text => {
            let pointer = reader.get_pointer_field(offset);
            match pointer.get_text(None) {
                Ok(text) => ReadValue::Text(text.to_str().unwrap_or("").to_owned()),
                Err(_) => ReadValue::Text(String::new()),
            }
        }
        FieldType::Data => {
            let pointer = reader.get_pointer_field(offset);
            match pointer.get_data(None) {
                Ok(data) => ReadValue::Data(data.to_vec()),
                Err(_) => ReadValue::Data(Vec::new()),
            }
        }
        FieldType::List(element) => {
            let pointer = reader.get_pointer_field(offset);
            let size = element_size(element);
            let list = match pointer.get_list(size, None) {
                Ok(list) => list,
                Err(_) => return ReadValue::List { count: 0, items: Vec::new() },
            };
            // capnp's public list API is typed, so the generic path reports
            // the list's LENGTH and leaves the items opaque: the honest
            // answer for a list it cannot itemize without a compiled type.
            ReadValue::List {
                count: list.len(),
                items: Vec::new(),
            }
        }
        FieldType::Struct(type_id) => {
            let pointer = reader.get_pointer_field(offset);
            match pointer.get_struct(None) {
                Ok(inner) => match tree.struct_info(*type_id) {
                    Some(structure) => ReadValue::Struct(read_struct(tree, &inner, structure)),
                    None => ReadValue::Opaque,
                },
                Err(_) => ReadValue::Opaque,
            }
        }
        _ => ReadValue::Opaque,
    }
}

/// The wire size of a list's elements (capnp's `ElementSize`).
fn element_size(element: &FieldType) -> layout::ElementSize {
    use layout::ElementSize as E;
    match element {
        FieldType::Bool => E::Bit,
        FieldType::Int(8) | FieldType::UInt(8) => E::Byte,
        FieldType::Int(16) | FieldType::UInt(16) => E::TwoBytes,
        FieldType::Int(32) | FieldType::UInt(32) | FieldType::Float(32) => E::FourBytes,
        FieldType::Int(_) | FieldType::UInt(_) | FieldType::Float(_) => E::EightBytes,
        _ => E::InlineComposite,
    }
}

/// Write a read value as a JSON object body (the contract's own key names).
pub fn write_json(value: &ReadValue, out: &mut String) {
    match value {
        ReadValue::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        ReadValue::Int(v) => out.push_str(&v.to_string()),
        ReadValue::UInt(v) => out.push_str(&v.to_string()),
        ReadValue::Float(v) => out.push_str(&v.to_string()),
        ReadValue::Text(v) => crate::json::write_str(out, v),
        ReadValue::Data(bytes) => {
            out.push('"');
            for byte in bytes {
                out.push_str(&alloc::format!("{byte:02x}"));
            }
            out.push('"');
        }
        ReadValue::List { count, items } => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            if items.is_empty() && *count > 0 {
                // The count is known even when the items are not: say so
                // rather than pretend the list is empty.
                out.push_str(&alloc::format!("\"<{count} items the generic path cannot itemize>\""));
            }
            out.push(']');
        }
        ReadValue::Struct(fields) => {
            out.push('{');
            for (index, (name, item)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                crate::json::write_key(out, name);
                write_json(item, out);
            }
            out.push('}');
        }
        ReadValue::Opaque => out.push_str("null"),
    }
}

/// Render a generic field list (the generic path's human view): the field
/// names the contract declares, in the contract's order.
pub fn render_fields(fields: &[(String, ReadValue)]) {
    let width = fields
        .iter()
        .map(|(name, _)| crate::present::display_width(name))
        .max()
        .unwrap_or(0);
    for (name, value) in fields {
        let mut text = String::new();
        write_json(value, &mut text);
        println!("{name:<width$}  {text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_without_a_schema_reads_as_nothing() {
        assert!(SchemaTree::decode(b"not a capnp message").is_none());
    }
}
