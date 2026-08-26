use crate::error::{CoreError, ErrorCode, Result};
use crate::model::{DecimalI64, SaveMetadata};
use crate::patch::SpanPatch;
use crate::xml::{ElementId, XmlDocument, XmlLimits};

#[derive(Debug, Clone)]
pub(crate) struct DescriptorAnchors {
    pub portrait: ElementId,
    pub character_name: ElementId,
    pub character_level: ElementId,
    pub slot_creation_timestamp: Option<ElementId>,
}

#[derive(Debug, Clone)]
pub struct DescriptorDocument {
    pub(crate) xml: XmlDocument,
    pub metadata: SaveMetadata,
    pub(crate) anchors: DescriptorAnchors,
}

pub fn parse_descriptor(bytes: Vec<u8>, limits: XmlLimits) -> Result<DescriptorDocument> {
    let xml = XmlDocument::parse(bytes, limits)?;
    let root = xml.root();
    if xml.name(root) != "SaveGameData" {
        return Err(CoreError::ambiguous(format!(
            "descriptor root is {}, expected SaveGameData",
            xml.name(root)
        )));
    }

    let portrait = xml.unique_direct_child(root, "portraitName")?;
    let character_name = xml.unique_direct_child(root, "characterName")?;
    let character_level = xml.unique_direct_child(root, "characterLevel")?;
    let slot_creation_timestamp =
        xml.optional_unique_direct_child(root, "slotCreationTimestamp")?;
    let game_version =
        optional_child_text(&xml, root, "gameVersion")?.unwrap_or_else(|| "Unknown".to_owned());
    let save_format = xml.child_text(root, "saveFileVersion")?;
    let compressed = parse_bool(&xml.child_text(root, "compressed")?, "compressed")?;
    let iron_mode = parse_bool(&xml.child_text(root, "isIronMode")?, "isIronMode")?;
    let autosave = match xml.optional_unique_direct_child(root, "autosave")? {
        Some(id) => parse_bool(&xml.simple_text(id)?, "autosave")?,
        None => false,
    };
    let enabled_mods = match xml.optional_unique_direct_child(root, "enabledMods")? {
        Some(container) => {
            if xml
                .children(container)
                .iter()
                .any(|child| xml.name(*child) != "st")
            {
                return Err(CoreError::ambiguous(
                    "enabledMods contains an unexpected structured member",
                ));
            }
            xml.direct_children_named(container, "st")
                .into_iter()
                .map(|id| xml.simple_text(id))
                .collect::<Result<Vec<_>>>()?
        }
        None => Vec::new(),
    };

    let metadata = SaveMetadata {
        character_name: xml.simple_text(character_name)?,
        portrait: xml.simple_text(portrait)?,
        game_version,
        save_format,
        character_level: parse_u32(&xml.simple_text(character_level)?, "characterLevel")?,
        compressed,
        iron_mode,
        autosave,
        difficulty: optional_child_text(&xml, root, "difficulty")?.unwrap_or_default(),
        location_description: optional_child_text(&xml, root, "locDesc")?.unwrap_or_default(),
        save_date: optional_child_text(&xml, root, "saveDate")?.unwrap_or_default(),
        slot_creation_timestamp: slot_creation_timestamp
            .map(|id| -> Result<DecimalI64> {
                Ok(DecimalI64::new(parse_i64(
                    &xml.simple_text(id)?,
                    "slotCreationTimestamp",
                )?))
            })
            .transpose()?,
        enabled_mods,
    };

    Ok(DescriptorDocument {
        xml,
        metadata,
        anchors: DescriptorAnchors {
            portrait,
            character_name,
            character_level,
            slot_creation_timestamp,
        },
    })
}

impl DescriptorDocument {
    pub(crate) const fn has_complete_write_shape(&self) -> bool {
        self.anchors.slot_creation_timestamp.is_some()
    }

    pub(crate) fn require_complete_write_shape(&self) -> Result<()> {
        if self.has_complete_write_shape() {
            Ok(())
        } else {
            Err(CoreError::ambiguous(
                "descriptor is missing slotCreationTimestamp; editing is disabled",
            ))
        }
    }

    pub(crate) fn name_patch(&self, value: &str) -> Result<SpanPatch> {
        self.xml.text_patch(
            self.anchors.character_name,
            value,
            "descriptor character name",
        )
    }

    pub(crate) fn portrait_patch(&self, value: &str) -> Result<SpanPatch> {
        self.xml
            .text_patch(self.anchors.portrait, value, "descriptor portrait")
    }

    pub(crate) fn level_patch(&self, value: u32) -> Result<SpanPatch> {
        self.xml.text_patch(
            self.anchors.character_level,
            &value.to_string(),
            "descriptor character level",
        )
    }

    pub(crate) fn slot_creation_patch(&self, value: i64) -> Result<SpanPatch> {
        let slot_creation_timestamp = self.anchors.slot_creation_timestamp.ok_or_else(|| {
            CoreError::ambiguous(
                "descriptor is missing slotCreationTimestamp; a save copy cannot be created",
            )
        })?;
        self.xml.text_patch(
            slot_creation_timestamp,
            &value.to_string(),
            "descriptor slot creation timestamp",
        )
    }
}

fn optional_child_text(xml: &XmlDocument, root: ElementId, name: &str) -> Result<Option<String>> {
    xml.optional_unique_direct_child(root, name)?
        .map(|id| xml.simple_text(id))
        .transpose()
}

fn parse_bool(value: &str, field: &str) -> Result<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(CoreError::new(
            ErrorCode::ValidationFailed,
            format!("{field} is not a boolean"),
        )),
    }
}

fn parse_u32(value: &str, field: &str) -> Result<u32> {
    value.trim().parse().map_err(|_| {
        CoreError::new(
            ErrorCode::ValidationFailed,
            format!("{field} is not a valid nonnegative integer"),
        )
    })
}

fn parse_i64(value: &str, field: &str) -> Result<i64> {
    value.trim().parse().map_err(|_| {
        CoreError::new(
            ErrorCode::ValidationFailed,
            format!("{field} is not a valid integer"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_descriptor_without_newer_metadata_remains_readable() {
        let descriptor = parse_descriptor(
            br#"<?xml version="1.0" ?><SaveGameData z="1"><portraitName>graphics/portraits/portrait_corporate05.png</portraitName><characterName>Tir Osman</characterName><saveFileVersion>0.5</saveFileVersion><characterLevel>7</characterLevel><compressed>false</compressed><isIronMode>false</isIronMode><difficulty>easy</difficulty><saveDate z="3">2022-07-30 16:59:48.832 UTC</saveDate><enabledMods z="5"></enabledMods></SaveGameData>"#.to_vec(),
            XmlLimits::default(),
        )
        .unwrap();

        assert_eq!(descriptor.metadata.character_name, "Tir Osman");
        assert_eq!(descriptor.metadata.game_version, "Unknown");
        assert_eq!(descriptor.metadata.save_format, "0.5");
        assert_eq!(descriptor.metadata.slot_creation_timestamp, None);
        assert!(!descriptor.has_complete_write_shape());
        let error = descriptor.require_complete_write_shape().unwrap_err();
        assert_eq!(error.code, ErrorCode::AmbiguousStructure);
    }
}
