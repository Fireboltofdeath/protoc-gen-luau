use std::{borrow::Cow, path::Path};

use prost_types::{
    field_descriptor_proto::{Label, Type},
    FieldDescriptorProto, FileDescriptorProto,
};

use crate::{
    generator::{file_path_export_name, ExportMap, MapType},
    if_builder::IfBuilder,
    string_builder::StringBuilder,
};

pub struct FieldGenerator<'a> {
    pub field_kind: FieldKind<'a>,
    pub export_map: &'a ExportMap,
    pub base_file: &'a FileDescriptorProto,
}

#[derive(Debug)]
pub enum FieldKind<'a> {
    Single(&'a FieldDescriptorProto),
    OneOf {
        name: String,
        fields: Vec<&'a FieldDescriptorProto>,
    },
}

impl FieldGenerator<'_> {
    // In a simple sense: will this be T? or T
    fn has_presence(&self) -> bool {
        match &self.field_kind {
            FieldKind::Single(field) => {
                if self.map_type().is_some() {
                    return false;
                }

                if field.label == Some(Label::Repeated as i32) {
                    return false;
                }

                field.label == Some(Label::Optional as i32)
                    || matches!(field.r#type(), Type::Message)
            }

            FieldKind::OneOf { .. } => true,
        }
    }

    pub fn name(&self) -> &str {
        match &self.field_kind {
            FieldKind::Single(field) => field.name(),
            FieldKind::OneOf { name, .. } => name,
        }
    }

    pub fn type_definition_no_presence(&self) -> String {
        match &self.field_kind {
            FieldKind::Single(field) => {
                if let Some(map_type) = self.map_type() {
                    format!(
                        "{{ [{}]: {} }}",
                        type_definition_of_field_descriptor(
                            &map_type.key,
                            self.export_map,
                            self.base_file
                        ),
                        type_definition_of_field_descriptor(
                            &map_type.value,
                            self.export_map,
                            self.base_file
                        ),
                    )
                } else {
                    let definition =
                        type_definition_of_field_descriptor(field, self.export_map, self.base_file);

                    if field.label.is_some() && field.label() == Label::Repeated {
                        format!("{{ {definition} }}")
                    } else {
                        definition
                    }
                }
            }

            FieldKind::OneOf { fields, .. } => {
                let variants = fields
                    .iter()
                    .map(|field| {
                        format!(
                            "{{ type: \"{}\", value: {} }}",
                            field.name(),
                            type_definition_of_field_descriptor(
                                field,
                                self.export_map,
                                self.base_file
                            )
                        )
                    })
                    .collect::<Vec<_>>();

                format!("({})", variants.join(" | "))
            }
        }
    }

    pub fn type_definition(&self) -> String {
        let mut definition = self.type_definition_no_presence();

        if self.has_presence() {
            definition.push('?');
        }

        definition
    }

    pub fn json_type(&self) -> String {
        match &self.field_kind {
            FieldKind::Single(_) => {
                let mut definition = self.type_definition_no_presence();
                definition.push('?');
                definition
            }

            FieldKind::OneOf { fields, .. } => {
                let variants = fields
                    .iter()
                    .map(|field| {
                        type_definition_of_field_descriptor(field, self.export_map, self.base_file)
                    })
                    .collect::<Vec<_>>();

                format!("({})?", variants.join(" | "))
            }
        }
    }

    pub fn should_encode(&self) -> String {
        let this = format!("self.{}", self.name());

        if self.has_presence() {
            return format!("{this} ~= nil");
        }

        match &self.field_kind {
            FieldKind::OneOf { .. } => unreachable!("OneOf has presence"),

            FieldKind::Single(field) => {
                if self.map_type().is_some() {
                    return format!("next({this}) ~= nil");
                }

                if field.label.is_some() && field.label() == Label::Repeated {
                    return format!("#{this} > 0");
                }

                // TODO: Remove default branch and explicitly type everything out
                match field.r#type() {
                    Type::Int32
                    | Type::Uint32
                    | Type::Int64
                    | Type::Uint64
                    | Type::Sint32
                    | Type::Sint64
                    | Type::Sfixed32
                    | Type::Sfixed64
                    | Type::Fixed32
                    | Type::Fixed64
                    | Type::Float
                    | Type::Double => {
                        format!("{this} ~= 0")
                    }
                    Type::String => format!("{this} ~= \"\""),
                    Type::Bool => this,
                    Type::Bytes => format!("buffer.len({this}) > 0"),
                    Type::Enum => format!(
                        "{this} ~= 0 or {this} ~= {}.fromNumber(0)",
                        type_definition_of_field_descriptor(field, self.export_map, self.base_file)
                    ),
                    Type::Message => unreachable!("Message has presence"),

                    Type::Group => unimplemented!("Group"),
                }
            }
        }
    }

    pub fn encode(&self) -> StringBuilder {
        let this = format!("self.{}", self.name());

        let mut encode = StringBuilder::new();
        encode.push(format!("if {} then", self.should_encode()));

        match &self.field_kind {
            FieldKind::Single(field) => {
                if let Some(map_type) = self.map_type() {
                    // Maps are { 1: key, 2: value }
                    encode.push(format!(
                        "for key: {}, value: {} in {this} do",
                        type_definition_of_field_descriptor(
                            &map_type.key,
                            self.export_map,
                            self.base_file
                        ),
                        type_definition_of_field_descriptor(
                            &map_type.value,
                            self.export_map,
                            self.base_file
                        ),
                    ));

                    encode.push("local mapBuffer = buffer.create(0)");
                    encode.push("local mapCursor = 0");

                    encode.push(
                        encode_field_descriptor_ignore_repeated(
                            &map_type.key,
                            self.export_map,
                            self.base_file,
                            "key",
                        )
                        .replace("cursor", "mapCursor")
                        .replace("output", "mapBuffer"),
                    );

                    encode.push(
                        encode_field_descriptor_ignore_repeated(
                            &map_type.value,
                            self.export_map,
                            self.base_file,
                            "value",
                        )
                        .replace("cursor", "mapCursor")
                        .replace("output", "mapBuffer"),
                    );

                    encode.push(format!("output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.lengthDelimited)", field.number()));
                    encode.push(
                        "output, cursor = proto.writeVarInt(output, cursor, buffer.len(mapBuffer))",
                    );
                    encode.push("output, cursor = proto.writeBuffer(output, cursor, mapBuffer)");

                    encode.push("end");
                } else if field.label.is_some() && field.label() == Label::Repeated {
                    encode.push(format!(
                        "for _, value: {} in {this} do",
                        type_definition_of_field_descriptor(field, self.export_map, self.base_file)
                    ));
                    encode.indent();

                    encode.push(encode_field_descriptor_ignore_repeated(
                        field,
                        self.export_map,
                        self.base_file,
                        "value",
                    ));

                    encode.dedent();
                    encode.push("end");
                } else {
                    encode.push(encode_field_descriptor_ignore_repeated(
                        field,
                        self.export_map,
                        self.base_file,
                        &this,
                    ));
                }
            }

            FieldKind::OneOf { fields, .. } => {
                let mut if_builder = IfBuilder::new();

                for field in fields {
                    if_builder.add_condition(
                        &format!("{this}.type == \"{}\"", field.name()),
                        |builder| {
                            builder.push(encode_field_descriptor_ignore_repeated(
                                field,
                                self.export_map,
                                self.base_file,
                                &format!("{this}.value"),
                            ));
                        },
                    );
                }

                encode.append(&mut if_builder.into_string_builder())
            }
        }

        encode.push("end");
        encode
    }

    // TODO: Use json_name
    pub fn json_encode(&self) -> StringBuilder {
        let this = format!("self.{}", self.name());
        let output = format!("output.{}", heck::AsLowerCamelCase(self.name()));

        let mut json_encode = StringBuilder::new();
        json_encode.push(format!("if {} then", self.should_encode()));

        match &self.field_kind {
            FieldKind::Single(field) => {
                if let Some(map_type) = self.map_type() {
                    json_encode.push(format!(
                        "local newOutput: {} = {{}}",
                        self.type_definition()
                    ));
                    json_encode.push(format!(
                        "for key: {}, value: {} in {this} do",
                        type_definition_of_field_descriptor(
                            &map_type.key,
                            self.export_map,
                            self.base_file
                        ),
                        type_definition_of_field_descriptor(
                            &map_type.value,
                            self.export_map,
                            self.base_file
                        )
                    ));
                    json_encode.push(format!(
                        "newOutput[{}] = {}",
                        json_encode_instruction_field_descriptor_ignore_repeated(
                            &map_type.key,
                            self.export_map,
                            self.base_file,
                            "key"
                        ),
                        json_encode_instruction_field_descriptor_ignore_repeated(
                            &map_type.value,
                            self.export_map,
                            self.base_file,
                            "value"
                        )
                    ));
                    json_encode.push("end");
                    json_encode.push(format!("{output} = newOutput"));
                } else if field.label.is_some() && field.label() == Label::Repeated {
                    json_encode.push(format!(
                        "local newOutput: {} = {{}}",
                        self.type_definition()
                    ));
                    json_encode.push(format!(
                        "for _, value: {} in {this} do",
                        type_definition_of_field_descriptor(field, self.export_map, self.base_file)
                    ));
                    json_encode.push(format!(
                        "table.insert(newOutput, {})",
                        json_encode_instruction_field_descriptor_ignore_repeated(
                            field,
                            self.export_map,
                            self.base_file,
                            "value"
                        )
                    ));
                    json_encode.push("end");
                    json_encode.push(format!("{output} = newOutput"));
                } else {
                    json_encode.push(format!(
                        "{output} = {}",
                        json_encode_instruction_field_descriptor_ignore_repeated(
                            field,
                            self.export_map,
                            self.base_file,
                            &this
                        )
                    ));
                }
            }

            FieldKind::OneOf { fields, .. } => {
                let mut if_builder = IfBuilder::new();

                for field in fields {
                    if_builder.add_condition(
                        &format!("{this}.type == \"{}\"", field.name()),
                        |builder| {
                            builder.push(format!(
                                "{output} = {}",
                                json_encode_instruction_field_descriptor_ignore_repeated(
                                    field,
                                    self.export_map,
                                    self.base_file,
                                    &format!("{this}.value")
                                )
                            ));
                        },
                    );
                }

                json_encode.append(&mut if_builder.into_string_builder())
            }
        }

        json_encode.push("end");
        json_encode
    }

    // TODO: For here and json_encode, they need to be camelCase
    pub fn json_decode(&self) -> StringBuilder {
        let mut json_decode = StringBuilder::new();

        for inner_field in self.inner_fields() {
            let real_name = inner_field.name();
            let json_name = heck::AsLowerCamelCase(real_name).to_string();

            let mut decode_name = |input_name: &str| {
                json_decode.push(format!("if input.{input_name} ~= nil then"));

                if let Some(map_info) = self.map_type() {
                    json_decode.push(format!(
                        "local newOutput: {} = {{}}",
                        self.type_definition()
                    ));
                    json_decode.push(format!("for key, value in input.{input_name} do"));
                    json_decode.push(format!(
                        "newOutput[{}] = {}",
                        json_decode_instruction_field_descriptor_ignore_repeated(
                            &map_info.key,
                            self.export_map,
                            self.base_file,
                            "key"
                        ),
                        json_decode_instruction_field_descriptor_ignore_repeated(
                            &map_info.value,
                            self.export_map,
                            self.base_file,
                            "value"
                        )
                    ));
                    json_decode.push("end");
                    json_decode.blank();
                    json_decode.push(format!("self.{real_name} = newOutput"));
                } else if inner_field.label.is_some() && inner_field.label() == Label::Repeated {
                    json_decode.push(format!(
                        "local newOutput: {} = {{}}",
                        self.type_definition()
                    ));
                    json_decode.push(format!(
                        "for _, value: {} in input.{input_name} do",
                        type_definition_of_field_descriptor(
                            inner_field,
                            self.export_map,
                            self.base_file
                        )
                    ));
                    json_decode.push(format!(
                        "table.insert(newOutput, {})",
                        json_decode_instruction_field_descriptor_ignore_repeated(
                            inner_field,
                            self.export_map,
                            self.base_file,
                            "value"
                        )
                    ));
                    json_decode.push("end");
                    json_decode.blank();
                    json_decode.push(format!("self.{real_name} = newOutput"));
                } else {
                    let json_decode_instruction =
                        json_decode_instruction_field_descriptor_ignore_repeated(
                            inner_field,
                            self.export_map,
                            self.base_file,
                            &format!("input.{input_name}"),
                        );

                    if let FieldKind::OneOf {
                        name: oneof_name, ..
                    } = &self.field_kind
                    {
                        json_decode.push(format!(
                        "self.{oneof_name} = {{ type = \"{real_name}\", value = {json_decode_instruction} }}",
                    ));
                    } else {
                        json_decode.push(format!("self.{real_name} = {json_decode_instruction}"));
                    }
                }

                json_decode.push("end");
                json_decode.blank();
            };

            decode_name(real_name);

            if real_name != json_name {
                decode_name(&json_name);
            }
        }

        json_decode
    }

    pub fn inner_fields(&self) -> Vec<&FieldDescriptorProto> {
        match &self.field_kind {
            FieldKind::Single(field) => vec![field],
            FieldKind::OneOf { fields, .. } => fields.clone(),
        }
    }

    pub fn map_type(&self) -> Option<&MapType> {
        let FieldKind::Single(field) = &self.field_kind else {
            return None;
        };

        let type_name = field.type_name();
        if type_name.is_empty() {
            return None;
        }

        assert!(
            type_name.starts_with('.'),
            "NYI: Relative type names: {type_name:?}"
        );

        let Some(export) = self.export_map.get(&type_name[1..]) else {
            return None;
        };

        export.map.as_ref()
    }

    pub fn default(&self) -> Cow<'static, str> {
        if self.has_presence() {
            return "nil".into();
        }

        match self.field_kind {
            FieldKind::Single(field) => {
                if field.label.is_some() && field.label() == Label::Repeated {
                    return "{}".into();
                }

                match field.r#type() {
                    Type::Int32
                    | Type::Uint32
                    | Type::Int64
                    | Type::Uint64
                    | Type::Fixed32
                    | Type::Fixed64
                    | Type::Sint32
                    | Type::Sint64
                    | Type::Sfixed32
                    | Type::Sfixed64
                    | Type::Float
                    | Type::Double => "0".into(),
                    Type::String => "\"\"".into(),
                    Type::Bool => "false".into(),
                    Type::Bytes => "buffer.create(0)".into(),
                    Type::Enum => format!(
                        "{}.fromNumber(0)",
                        type_definition_of_field_descriptor(field, self.export_map, self.base_file)
                    )
                    .into(),
                    Type::Message => format!(
                        "{}.new()",
                        type_definition_of_field_descriptor(field, self.export_map, self.base_file)
                    )
                    .into(),
                    Type::Group => unimplemented!("Group"),
                }
            }

            FieldKind::OneOf { .. } => "nil".into(),
        }
    }
}

fn type_definition_of_field_descriptor(
    field: &FieldDescriptorProto,
    export_map: &ExportMap,
    base_file: &FileDescriptorProto,
) -> String {
    match field.r#type() {
        Type::Int32
        | Type::Uint32
        | Type::Int64
        | Type::Uint64
        | Type::Fixed32
        | Type::Fixed64
        | Type::Sint32
        | Type::Sint64
        | Type::Sfixed32
        | Type::Sfixed64
        | Type::Float
        | Type::Double => "number".to_owned(),
        Type::String => "string".to_owned(),
        Type::Bool => "boolean".to_owned(),
        Type::Bytes => "buffer".to_owned(),
        Type::Enum | Type::Message => {
            let type_name = field.type_name();
            assert!(
                type_name.starts_with('.'),
                "NYI: Relative type names: {type_name:?}"
            );

            let type_name = &type_name[1..];

            let mut segments: Vec<&str> = type_name.split('.').collect();
            let just_type = segments.pop().unwrap();
            let package = segments.join(".");

            let export = export_map
                .get(&format!("{package}.{just_type}"))
                .unwrap_or_else(|| panic!("couldn't find export {package}.{just_type}"));

            if export.path == Path::new(base_file.name()).with_extension("") {
                format!("{}{just_type}", export.prefix)
            } else {
                format!(
                    "{}.{}{just_type}",
                    file_path_export_name(&export.path),
                    export.prefix,
                )
            }
        }

        Type::Group => unimplemented!("Group"),
    }
}

#[derive(Clone, Copy)]
pub enum WireType {
    Varint,
    LengthDelimited,
    I32,
    I64,
}

pub fn wire_type_of_field_descriptor(field: &FieldDescriptorProto) -> WireType {
    match field.r#type() {
        Type::Int32
        | Type::Uint32
        | Type::Int64
        | Type::Uint64
        | Type::Sint32
        | Type::Sint64
        | Type::Enum
        | Type::Bool => WireType::Varint,
        Type::Float | Type::Fixed32 | Type::Sfixed32 => WireType::I32,
        Type::Double | Type::Fixed64 | Type::Sfixed64 => WireType::I64,
        Type::String | Type::Bytes | Type::Message => WireType::LengthDelimited,
        Type::Group => unimplemented!("Group"),
    }
}

fn encode_field_descriptor_ignore_repeated(
    field: &FieldDescriptorProto,
    export_map: &ExportMap,
    base_file: &FileDescriptorProto,
    value_var: &str,
) -> String {
    match field.r#type() {
        Type::Int32 | Type::Uint32 | Type::Int64 | Type::Uint64 => [
            format!(
                "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.varint)",
                field.number()
            ),
            format!("output, cursor = proto.writeVarInt(output, cursor, {value_var})"),
        ]
        .join("\n"),

        Type::Sint32 | Type::Sint64 => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.varint)",
                    field.number()
                ),
                format!(
                    "output, cursor = proto.writeVarInt(output, cursor, proto.encodeZigZag({value_var}))",
                ),
            ]
            .join("\n")
        }

        Type::Float => [
            format!(
                "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.i32)",
                field.number()
            ),
            format!("output, cursor = proto.writeFloat(output, cursor, {value_var})"),
        ]
        .join("\n"),

        Type::Double => [
            format!(
                "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.i64)",
                field.number()
            ),
            format!("output, cursor = proto.writeDouble(output, cursor, {value_var})"),
        ]
        .join("\n"),

        Type::String => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.lengthDelimited)",
                    field.number()
                ),
                format!("output, cursor = proto.writeString(output, cursor, {value_var})"),
            ]
            .join("\n")
        }

        Type::Bool => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.varint)",
                    field.number()
                ),
                format!(
                    "output, cursor = proto.writeVarInt(output, cursor, if {value_var} then 1 else 0)",
                ),
            ]
            .join("\n")
        }

        Type::Bytes => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.lengthDelimited)",
                    field.number()
                ),
                format!("output, cursor = proto.writeBuffer(output, cursor, {value_var})"),
            ]
            .join("\n")
        }

        Type::Enum => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.varint)",
                    field.number()
                ),
                format!(
                    "output, cursor = proto.writeVarInt(output, cursor, {}.toNumber({value_var}))",
                    type_definition_of_field_descriptor(field, export_map, base_file)
                ),
            ]
            .join("\n")
        }

        Type::Message => {
            [
                format!(
                    "local encoded = {}.encode({value_var})",
                    type_definition_of_field_descriptor(field, export_map, base_file)
                ),
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.lengthDelimited)",
                    field.number()
                ),
                "output, cursor = proto.writeVarInt(output, cursor, buffer.len(encoded))".to_owned(),
                "output, cursor = proto.writeBuffer(output, cursor, encoded)".to_owned(),
            ]
            .join("\n")
        }

        Type::Fixed32 => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.i32)",
                    field.number()
                ),
                format!("output, cursor = proto.writeFixed32(output, cursor, {value_var})"),
            ]
            .join("\n")
        },

        Type::Fixed64 => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.i64)",
                    field.number()
                ),
                format!("output, cursor = proto.writeFixed64(output, cursor, {value_var})"),
            ].join("\n")
        },

        Type::Sfixed32 => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.i32)",
                    field.number()
                ),
                format!("output, cursor = proto.writeFixed32(output, cursor, proto.encodeZigZag({value_var}))"),
            ].join("\n")
        },

        Type::Sfixed64 => {
            [
                format!(
                    "output, cursor = proto.writeTag(output, cursor, {}, proto.wireTypes.i64)",
                    field.number()
                ),
                format!("output, cursor = proto.writeFixed64(output, cursor, proto.encodeZigZag({value_var}))"),
            ].join("\n")
        },

        Type::Group => unimplemented!("Group"),
    }
}

fn json_encode_instruction_field_descriptor_ignore_repeated(
    field: &FieldDescriptorProto,
    export_map: &ExportMap,
    base_file: &FileDescriptorProto,
    value_var: &str,
) -> String {
    match field.r#type() {
        Type::Int32
        | Type::Int64
        | Type::Uint32
        | Type::Uint64
        | Type::Fixed32
        | Type::Fixed64
        | Type::Sint32
        | Type::Sint64
        | Type::Sfixed32
        | Type::Sfixed64
        | Type::Bool
        | Type::String => value_var.to_owned(),
        Type::Float | Type::Double => format!("proto.json.serializeNumber({value_var})"),
        Type::Bytes => format!("proto.json.serializeBuffer({value_var})"),
        Type::Enum => format!(
            "if typeof({value_var}) == \"number\" then {value_var} else {}.toNumber({value_var})",
            type_definition_of_field_descriptor(field, export_map, base_file)
        ),
        Type::Message => format!(
            "{}.jsonEncode({value_var})",
            type_definition_of_field_descriptor(field, export_map, base_file)
        ),
        Type::Group => unimplemented!("Group"),
    }
}

fn json_decode_instruction_field_descriptor_ignore_repeated(
    field: &FieldDescriptorProto,
    export_map: &ExportMap,
    base_file: &FileDescriptorProto,
    value_var: &str,
) -> String {
    match field.r#type() {
        Type::Int32
        | Type::Int64
        | Type::Uint32
        | Type::Uint64
        | Type::Fixed32
        | Type::Fixed64
        | Type::Sfixed32
        | Type::Sfixed64
        | Type::Sint32
        | Type::Sint64
        | Type::Bool
        | Type::String => value_var.to_owned(),
        Type::Float | Type::Double => format!("proto.json.deserializeNumber({value_var})"),
        Type::Bytes => format!("proto.json.deserializeBuffer({value_var})"),
        Type::Enum => format!(
            "if typeof({value_var}) == \"number\" then ({qualified_enum}.fromNumber({value_var}) \
                or {value_var}) else ({qualified_enum}.fromName({value_var}) or {value_var})",
            qualified_enum = type_definition_of_field_descriptor(field, export_map, base_file)
        ),
        Type::Message => format!(
            "{}.jsonDecode({value_var})",
            type_definition_of_field_descriptor(field, export_map, base_file)
        ),
        Type::Group => unimplemented!("Group"),
    }
}

fn decode_instruction_field_descriptor_ignore_repeated(
    field: &FieldDescriptorProto,
    export_map: &ExportMap,
    base_file: &FileDescriptorProto,
) -> Cow<'static, str> {
    match field.r#type() {
        Type::Int32
        | Type::Uint32
        | Type::Int64
        | Type::Uint64
        | Type::Fixed32
        | Type::Fixed64
        | Type::Float
        | Type::Double
        | Type::Bytes => "value".into(),

        Type::Sint32 | Type::Sint64 | Type::Sfixed32 | Type::Sfixed64 => {
            "proto.decodeZigZag(value)".into()
        }

        Type::Bool => "value ~= 0".into(),

        Type::String => "buffer.tostring(value)".into(),

        Type::Enum => format!(
            "{}.fromNumber(value) or value",
            type_definition_of_field_descriptor(field, export_map, base_file)
        )
        .into(),

        Type::Message => format!(
            "{}.decode(value)",
            type_definition_of_field_descriptor(field, export_map, base_file)
        )
        .into(),

        Type::Group => unimplemented!("Group"),
    }
}

// TODO: Variable for "value" instead of replace
pub fn decode_field(
    this: &str,
    field: &FieldDescriptorProto,
    export_map: &ExportMap,
    base_file: &FileDescriptorProto,
    map_type: Option<&MapType>,
    is_oneof: bool,
) -> StringBuilder {
    let mut decode = StringBuilder::new();

    if let Some(map_type) = map_type {
        decode.push(format!(
            "local mapKey: {}",
            type_definition_of_field_descriptor(&map_type.key, export_map, base_file),
        ));
        decode.push(format!(
            "local mapValue: {}",
            type_definition_of_field_descriptor(&map_type.value, export_map, base_file),
        ));
        decode.blank();

        decode.push(
            wire_type_header(wire_type_of_field_descriptor(&map_type.key))
                .replace("value", "readMapKey"),
        );

        decode.append(
            &mut decode_field("mapKey", &map_type.key, export_map, base_file, None, false)
                .replace("value", "readMapKey"),
        );
        decode.blank();

        decode.push(
            wire_type_header(wire_type_of_field_descriptor(&map_type.value))
                .replace("value", "readMapValue"),
        );

        decode.append(
            &mut decode_field(
                "mapValue",
                &map_type.value,
                export_map,
                base_file,
                None,
                false,
            )
            .replace("value", "readMapValue"),
        );
        decode.blank();

        decode.push(format!("{this}[mapKey] = mapValue"));
    } else {
        match field.r#type() {
            Type::Float => {
                decode.push("local value");
                decode.push("value, cursor = proto.readFloat(input, cursor)");
            }

            Type::Double => {
                decode.push("local value");
                decode.push("value, cursor = proto.readDouble(input, cursor)");
            }

            Type::Fixed32 | Type::Sfixed32 => {
                decode.push("local value");
                decode.push("value, cursor = proto.readFixed32(input, cursor)");
            }

            Type::Fixed64 | Type::Sfixed64 => {
                decode.push("local value");
                decode.push("value, cursor = proto.readFixed64(input, cursor)");
            }

            _ => {}
        }

        if field.label.is_some() && field.label() == Label::Repeated {
            decode.push(format!(
                "table.insert({this}, {})",
                decode_instruction_field_descriptor_ignore_repeated(field, export_map, base_file)
            ));
        } else if is_oneof {
            decode.push(format!(
                "{this} = {{ type = \"{}\", value = {} }}",
                field.name(),
                decode_instruction_field_descriptor_ignore_repeated(field, export_map, base_file)
            ));
        } else {
            decode.push(format!(
                "{this} = {}",
                decode_instruction_field_descriptor_ignore_repeated(field, export_map, base_file)
            ));
        }
    }

    decode
}

// TODO: Use this in MESSAGE
pub fn wire_type_header(wire_type: WireType) -> &'static str {
    match wire_type {
        WireType::Varint => "local value\nvalue, cursor = proto.readVarInt(input, cursor)",
        WireType::LengthDelimited => "local value\nvalue, cursor = proto.readBuffer(input, cursor)",
        WireType::I32 | WireType::I64 => "",
    }
}
