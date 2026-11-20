/// A fieldless enum with stable wire strings: `ALL`, `COUNT`, `VALUES`, `as_str`, `parse`,
/// `FromStr`, `Display` and string serde.
///
/// ```
/// loa_util::str_enum! { pub Mode { Fast => "fast", Slow => "slow" } }
/// assert_eq!("slow".parse::<Mode>(), Ok(Mode::Slow));
/// ```
#[macro_export]
macro_rules! str_enum {
    ($(#[$meta:meta])* $vis:vis $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
        $vis enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub const COUNT: usize = Self::ALL.len();
            pub const VALUES: &'static [&'static str] = &[$($text),+];

            pub const fn as_str(self) -> &'static str { match self { $($name::$variant => $text),+ } }
            pub fn parse(text: &str) -> Option<Self> { match text { $($text => Some($name::$variant),)+ _ => None } }
        }

        impl ::std::str::FromStr for $name {
            type Err = String;
            fn from_str(text: &str) -> Result<Self, String> {
                Self::parse(text).ok_or_else(|| format!("unknown {} `{text}`", stringify!($name)))
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result { f.write_str(self.as_str()) }
        }

        impl $crate::__serde::Serialize for $name {
            fn serialize<S: $crate::__serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(self.as_str()) }
        }

        impl<'de> $crate::__serde::Deserialize<'de> for $name {
            fn deserialize<D: $crate::__serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = <String as $crate::__serde::Deserialize>::deserialize(d)?;
                Self::parse(&text).ok_or_else(|| <D::Error as $crate::__serde::de::Error>::unknown_variant(&text, Self::VALUES))
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use crate::Semantic;

    #[test]
    fn variants_round_trip_through_every_form() {
        assert_eq!(Semantic::COUNT, 7);
        for (i, &v) in Semantic::ALL.iter().enumerate() {
            assert_eq!(v as usize, i);
            assert_eq!(v.as_str(), Semantic::VALUES[i]);
            assert_eq!(v.to_string().parse::<Semantic>(), Ok(v));
            let json = serde_json::to_string(&v).unwrap();
            assert_eq!(json, format!("\"{}\"", v.as_str()));
            assert_eq!(serde_json::from_str::<Semantic>(&json).unwrap(), v);
        }
        assert!("Ok".parse::<Semantic>().is_err());
        assert!(serde_json::from_str::<Semantic>("\"nope\"").is_err());
    }
}
