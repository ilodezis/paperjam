//! UUIDv7 newtypes for every entity id. Time-ordered, stored as TEXT.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::sqlite::{SqliteTypeInfo, SqliteValueRef};
use sqlx::{Database, Decode, Encode, Sqlite, Type};
use uuid::Uuid;

macro_rules! id_type {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
            #[serde(transparent)]
            pub struct $name(Uuid);

            impl $name {
                pub fn new() -> Self {
                    Self(Uuid::now_v7())
                }

                pub fn uuid(self) -> Uuid {
                    self.0
                }
            }

            impl Default for $name {
                fn default() -> Self {
                    Self::new()
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    self.0.hyphenated().fmt(f)
                }
            }

            impl FromStr for $name {
                type Err = uuid::Error;
                fn from_str(s: &str) -> Result<Self, Self::Err> {
                    Uuid::from_str(s).map(Self)
                }
            }

            impl Type<Sqlite> for $name {
                fn type_info() -> SqliteTypeInfo {
                    <String as Type<Sqlite>>::type_info()
                }

                fn compatible(ty: &SqliteTypeInfo) -> bool {
                    <String as Type<Sqlite>>::compatible(ty)
                }
            }

            impl Encode<'_, Sqlite> for $name {
                fn encode_by_ref(
                    &self,
                    buf: &mut <Sqlite as Database>::ArgumentBuffer,
                ) -> Result<IsNull, BoxDynError> {
                    <String as Encode<'_, Sqlite>>::encode(self.to_string(), buf)
                }
            }

            impl<'r> Decode<'r, Sqlite> for $name {
                fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
                    let text = <&str as Decode<Sqlite>>::decode(value)?;
                    Ok(text.parse()?)
                }
            }
        )+
    };
}

id_type!(
    DocumentId,
    PersonId,
    CorrespondentId,
    TagId,
    TypeId,
    UserId,
    TokenId,
    JobId,
    ClientId,
    PasskeyId,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_time_ordered() {
        let a = DocumentId::new();
        let b = DocumentId::new();
        assert!(a < b);
        assert!(a.to_string() < b.to_string());
    }

    #[test]
    fn roundtrip_string() {
        let id = PersonId::new();
        let parsed: PersonId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
        assert!("not-a-uuid".parse::<PersonId>().is_err());
    }

    #[test]
    fn serde_is_a_plain_string() {
        let id = TagId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        let back: TagId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
