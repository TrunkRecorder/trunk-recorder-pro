//! [`choice!`]: the config's closed choices — a value read in any case, saved
//! in one, and anything not listed refused (as a wrong type is).

/// A closed choice: each variant with its names in the config (the first is
/// how it is saved), read in any case; "" reads as the default (the variant
/// marked so).
macro_rules! choice {
    ($(#[$m:meta])* pub enum $name:ident { $( $(#[$vm:meta])* $v:ident = $($lit:literal)|+ ),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub enum $name { $( $(#[$vm])* $v ),+ }
        impl $name {
            /// As the config writes it.
            pub fn name(self) -> &'static str {
                match self { $( $name::$v => choice!(@first $($lit)|+) ),+ }
            }
        }
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.name())
            }
        }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = <String as serde::Deserialize>::deserialize(d)?;
                let t = text.trim();
                if t.is_empty() {
                    return Ok(Self::default());
                }
                $( if $( t.eq_ignore_ascii_case($lit) )||+ { return Ok($name::$v); } )+
                Err(serde::de::Error::unknown_variant(t, &[$( choice!(@first $($lit)|+) ),+]))
            }
        }
    };
    (@first $first:literal $(| $rest:literal)*) => { $first };
}
