#[cfg(not(feature = "migrations"))]
use core::fmt::Formatter;
#[cfg(feature = "migrations")]
use core::fmt::{Display, Formatter};
#[cfg(feature = "migrations")]
use core::str::FromStr;

use borsh::{BorshDeserialize, BorshSerialize};
use borsh_ext::BorshSerializeExt;
use data_encoding::HEXUPPER;
use namada_core::storage::Key;
#[cfg(feature = "migrations")]
use namada_macros::derive_borshdeserializer;
#[cfg(feature = "migrations")]
use namada_migrations::TypeHash;
#[cfg(feature = "migrations")]
use namada_migrations::*;
use regex::Regex;
use serde::de::{Error, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(feature = "migrations")]
/// The maximum number of character printed per value.
const PRINTLN_CUTOFF: usize = 300;

pub trait DBUpdateVisitor {
    fn read(&self, key: &Key) -> Option<Vec<u8>>;
    fn write(&mut self, key: &Key, value: impl AsRef<[u8]>);
    fn delete(&mut self, key: &Key);
    fn get_pattern(&self, pattern: Regex) -> Vec<(String, Vec<u8>)>;
}
#[derive(Clone, BorshSerialize, BorshDeserialize)]
 enum UpdateBytes {
    Raw {
        to_write: Vec<u8>,
        serialized: Vec<u8>,
    },
    Serialized {
        bytes: Vec<u8>
    }
}


#[derive(Clone, BorshSerialize, BorshDeserialize)]
/// A value to be added to the database that can be
/// validated.
pub struct UpdateValue{
    type_hash: [u8; 32],
    bytes: UpdateBytes
}

#[cfg(feature = "migrations")]
impl UpdateValue {

    pub fn raw<T>(value: T) -> Self
    where
        T: TypeHash + AsRef<[u8]> + BorshSerialize + BorshDeserialize
    {
        Self {
            type_hash: T::HASH,
            bytes: UpdateBytes::Raw {
                to_write: value.as_ref().to_vec(),
                serialized: value.serialize_to_vec(),
            },
        }
    }

    pub fn is_raw(&self) -> bool {
        matches!(self.bytes, UpdateBytes::Raw{..})
    }

    fn bytes(&self) -> &[u8] {
        match &self.bytes {
            UpdateBytes::Raw { serialized, .. } => serialized,
            UpdateBytes::Serialized {bytes } => bytes,
        }
    }

    /// The value to write to storage
    fn to_write(&self) -> Vec<u8> {
        match &self.bytes {
            UpdateBytes::Raw {to_write, .. } => to_write.clone(),
            UpdateBytes::Serialized { bytes } => bytes.clone(),
        }
    }
}

#[cfg(feature = "migrations")]
impl<T: TypeHash + BorshSerialize + BorshDeserialize> From<T> for UpdateValue {
    fn from(value: T) -> Self {
        Self {
            type_hash: T::HASH,
            bytes: UpdateBytes::Serialized {bytes: value.serialize_to_vec()},
        }
    }
}

#[derive(Default)]
struct UpdateValueVisitor;

impl<'de> Visitor<'de> for UpdateValueVisitor {
    type Value = UpdateValue;

    fn expecting(&self, formatter: &mut Formatter) -> core::fmt::Result {
        formatter.write_str(
            "a hex encoded series of bytes that borsh decode to an \
             UpdateValue.",
        )
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        UpdateValue::try_from_slice(
            &HEXUPPER
                .decode(v.as_bytes())
                .map_err(|e| E::custom(e.to_string()))?,
        )
        .map_err(|e| E::custom(e.to_string()))
    }
}

impl Serialize for UpdateValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_bytes = HEXUPPER.encode(&self.serialize_to_vec());
        Serialize::serialize(&hex_bytes, serializer)
    }
}

impl<'de> Deserialize<'de> for UpdateValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UpdateValueVisitor)
    }
}
#[derive(Clone, Serialize, Deserialize)]
/// An update to the database
pub enum DbUpdateType {
    Add {
        key: Key,
        value: UpdateValue,
        force: bool,
    },
    Delete(Key),
    RepeatAdd {
        pattern: String,
        value: UpdateValue,
        force: bool,
    },
    RepeatDelete(String),
}

#[cfg(feature = "migrations")]
impl DbUpdateType
{
    /// Get the key or pattern being modified as string
    pub fn pattern(&self) -> String {
        match self {
            DbUpdateType::Add { key, .. } => key.to_string(),
            DbUpdateType::Delete(key) => key.to_string(),
            DbUpdateType::RepeatAdd { pattern, .. } => pattern.to_string(),
            DbUpdateType::RepeatDelete(pattern) => pattern.to_string(),
        }
    }

    fn is_force(&self) -> bool {
        match self{
            DbUpdateType::Add{force, ..} => *force,
            DbUpdateType::RepeatAdd {force, ..} => *force,
            _ => false,
        }
    }
    fn formatted_bytes(&self) -> String {
        match self {
            DbUpdateType::Add {value, ..} | DbUpdateType::RepeatAdd {value, ..} => {
                if value.to_write().len() > PRINTLN_CUTOFF {
                    format!("{:?} ...", &value.bytes()[..PRINTLN_CUTOFF])
                } else {
                    format!("{:?}", value.bytes())
                }
            },
            _ => String::default(),
        }
    }

    /// Validate that the contained value deserializes correctly given its data
    /// hash and the value is not "raw". Return the string formatted value and,
    /// if the value is not "raw", the deserializer function.
    pub fn validate(&self) -> eyre::Result<(String, Option<CbFromByteArrayToTypeName>)> {
        // skip all checks if force == true
        if self.is_force() {
            return Ok((self.formatted_bytes(), None));
        }
        let key_or_pattern = self.pattern();
        match self {
            DbUpdateType::RepeatAdd { value, .. }
            | DbUpdateType::Add { value, .. } => {
                let deserializer =
                    namada_migrations::get_deserializer(&value.type_hash)
                        .ok_or_else(|| {
                            eyre::eyre!(
                                "Type hash {:?} did not correspond to a \
                                 deserializer in TYPE_DESERIALIZERS.",
                                value.type_hash
                            )
                        })?;
                let deserialized = deserializer(value.bytes().to_vec()).ok_or_else(|| {
                        eyre::eyre!(
                        "The value {:?} for key/pattern {} could not be successfully deserialized",
                        value.bytes(),
                        key_or_pattern,
                    )
                    })?;
                let deserializer = value.is_raw().then_some(deserializer);
                if deserialized.len() > PRINTLN_CUTOFF {
                    Ok((format!("{} ...", deserialized.chars().take(PRINTLN_CUTOFF).collect::<String>()), deserializer))
                } else {
                    Ok((deserialized, deserializer))
                }
            }
            DbUpdateType::Delete(_) | DbUpdateType::RepeatDelete(_) => Ok((String::default(), None)),
        }
    }

    /// Validate a DB change and persist it if so. The debug representation of
    /// the new value is returned for logging purposes.
    #[allow(dead_code)]
    pub fn update<DB: DBUpdateVisitor>(
        &self,
        db: &mut DB,
    ) -> eyre::Result<UpdateStatus> {
        match self {
            Self::Add { key, value, .. } => {
                let (deserialized, deserializer) = self.validate()?;
                if let (Some(prev), Some(des)) = (db.read(key), deserializer) {
                    des(prev).ok_or_else(|| {
                        eyre::eyre!(
                            "The previous value under the key {} did not \
                             have the same type as that provided: Input \
                             was {}",
                            key,
                            deserialized
                        )
                    })?;
                }
                db.write(key, &value.to_write());
                Ok(UpdateStatus::Add(vec![(key.to_string(), deserialized)]))
            }
            Self::Delete(key) => {
                db.delete(key);
                Ok(UpdateStatus::Deleted(vec![key.to_string()]))
            }
            DbUpdateType::RepeatAdd {
                pattern,
                value,
                ..
            } => {
                let pattern = Regex::new(pattern).unwrap();
                let mut pairs = vec![];
                let (deserialized, deserializer) = self.validate()?;
                for (key, prev) in db.get_pattern(pattern.clone()) {
                    if let Some(des) = deserializer
                    {
                        des(prev).ok_or_else(|| {
                            eyre::eyre!(
                                "The previous value under the key {} did not \
                                 have the same type as that provided: Input \
                                 was {}",
                                key,
                                deserialized,
                            )
                        })?;
                        pairs.push((key.to_string(), deserialized.clone()));
                    } else {
                        pairs.push((
                            key.to_string(),
                            deserialized.clone(),
                        ));
                    }
                    db.write(&Key::from_str(&key).unwrap(), value.to_write());
                }
                Ok(UpdateStatus::Add(pairs))
            }
            DbUpdateType::RepeatDelete(pattern) => {
                let pattern = Regex::new(pattern).unwrap();
                Ok(UpdateStatus::Deleted(
                    db.get_pattern(pattern.clone())
                        .into_iter()
                        .map(|(key, _)| {
                            db.delete(&Key::from_str(&key).unwrap());
                            key
                        })
                        .collect(),
                ))
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct DbChanges {
    pub changes: Vec<DbUpdateType>,
}

#[cfg(feature = "migrations")]
impl Display for DbUpdateType {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            DbUpdateType::Add { key, value, .. } => {
                let (formatted, _) = match self.validate() {
                    Ok(f) => f,
                    Err(e) => return f.write_str(&e.to_string())
                };

                f.write_str(&format!(
                    "Write to key: <{}> with {}value: {}",
                    key,
                    value.is_raw().then_some("raw " ).unwrap_or_default(),
                    formatted
                ))
            }
            DbUpdateType::Delete(key) => {
                f.write_str(&format!("Delete key: <{}>", key))
            }
            DbUpdateType::RepeatAdd {
                pattern,
                value,
                ..
            } => {
                let (formatted, _) = match self.validate() {
                    Ok(f) => f,
                    Err(e) => return f.write_str(&e.to_string())
                };
                f.write_str(&format!(
                    "Write to pattern: <{}> with {}value: {}",
                    pattern,
                    value.is_raw().then_some("raw ").unwrap_or_default(),
                    formatted,
                ))
            }
            DbUpdateType::RepeatDelete(pattern) => {
                f.write_str(&format!("Delete pattern: <{}>", pattern))
            }
        }
    }
}

pub enum UpdateStatus {
    Deleted(Vec<String>),
    Add(Vec<(String, String)>),
}

#[cfg(feature = "migrations")]
impl Display for UpdateStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Deleted(keys) => {
                for key in keys {
                    f.write_str(&format!("Deleting key <{}>\n", key))?;
                }
            }
            Self::Add(pairs) => {
                for (k, v) in pairs {
                    f.write_str(&format!(
                        "Writing key <{}> with value: {}\n",
                        k, v
                    ))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(feature = "migrations")]
derive_borshdeserializer!(Vec::<u8>);
#[cfg(feature = "migrations")]
derive_borshdeserializer!(Vec::<String>);
#[cfg(feature = "migrations")]
derive_borshdeserializer!(u64);
