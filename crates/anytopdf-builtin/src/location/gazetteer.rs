//! Offline gazetteer built from GeoNames `cities1000` (CC BY 4.0), embedded in
//! the binary. See `scripts/build-geonames.py` for how the table is produced.

use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::OnceLock;

static DATA: &[u8] = include_bytes!("../../data/geonames-cities1000.tsv.gz");

/// Attribution GeoNames' licence requires wherever its data is redistributed.
pub(crate) const ATTRIBUTION: &str = "GeoNames (geonames.org), CC BY 4.0";

/// Cities smaller than this are never matched in text: small towns share
/// their names with too many ordinary words and people.
const TEXT_MIN_POPULATION: u32 = 100_000;

/// Longest place name, in words, that text matching tries.
const MAX_NAME_TOKENS: usize = 5;

/// Single-word names that are far more often ordinary words or first names
/// than the place, compared case-insensitively.
const STOPWORDS: &[&str] = &[
    "ajax",
    "alexandra",
    "ann",
    "aurora",
    "barking",
    "batman",
    "bath",
    "brandon",
    "brent",
    "brits",
    "buffalo",
    "cary",
    "chad",
    "chandler",
    "charlotte",
    "columbia",
    "commonwealth",
    "concord",
    "corona",
    "darwin",
    "delta",
    "dudley",
    "edison",
    "elizabeth",
    "enterprise",
    "eugene",
    "everett",
    "garland",
    "george",
    "gilbert",
    "hamilton",
    "hassan",
    "henderson",
    "hub",
    "independence",
    "irving",
    "jackson",
    "jordan",
    "kara",
    "kennedy",
    "lafayette",
    "lincoln",
    "lowell",
    "madison",
    "male",
    "mango",
    "mansfield",
    "mary",
    "mesa",
    "metro",
    "midland",
    "milton",
    "mobile",
    "montgomery",
    "nancy",
    "natal",
    "newton",
    "nice",
    "nigel",
    "norman",
    "oral",
    "orange",
    "paradise",
    "pest",
    "preston",
    "providence",
    "reading",
    "regina",
    "richardson",
    "rodriguez",
    "saga",
    "salvador",
    "santos",
    "shaping",
    "split",
    "springs",
    "surprise",
    "sutton",
    "thornton",
    "tours",
    "toyota",
    "tyler",
    "tyre",
    "vaughan",
    "victoria",
    "vista",
    "vladimir",
    "warren",
];

/// Common spellings GeoNames does not carry as primary names.
const ALIASES: &[(&str, Alias)] = &[
    ("USA", Alias::Country("US")),
    ("U S A", Alias::Country("US")),
    ("United States of America", Alias::Country("US")),
    ("UK", Alias::Country("GB")),
    ("U K", Alias::Country("GB")),
    ("Great Britain", Alias::Country("GB")),
    ("Britain", Alias::Country("GB")),
    ("England", Alias::Region("GB", "ENG")),
    ("Scotland", Alias::Region("GB", "SCT")),
    ("Wales", Alias::Region("GB", "WLS")),
    ("Northern Ireland", Alias::Region("GB", "NIR")),
    ("UAE", Alias::Country("AE")),
    ("New York", Alias::Place("New York City", "US")),
    ("NYC", Alias::Place("New York City", "US")),
    ("Washington DC", Alias::Place("Washington, D.C.", "US")),
    ("Washington D C", Alias::Place("Washington, D.C.", "US")),
];

#[derive(Debug, Clone, Copy)]
enum Alias {
    Country(&'static str),
    Region(&'static str, &'static str),
    Place(&'static str, &'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Target {
    Place(u32),
    Region(u32),
    Country(u32),
}

struct Place {
    name: Box<str>,
    lat_e3: i32,
    lon_e3: i32,
    region: u32,
    population: u32,
}

struct Region {
    country: u32,
    code: Box<str>,
    name: Option<Box<str>>,
}

struct Country {
    code: Box<str>,
    name: Box<str>,
}

pub(crate) struct Gazetteer {
    countries: Vec<Country>,
    regions: Vec<Region>,
    places: Vec<Place>,
    names: HashMap<String, Target>,
    upper: HashMap<String, Target>,
}

/// A resolved place, ready to be written into annotation attributes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlaceInfo {
    pub kind: &'static str,
    pub name: String,
    pub region: Option<String>,
    pub country: String,
    pub country_code: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub population: Option<u32>,
}

impl PlaceInfo {
    /// "City, Region, Country", skipping parts that repeat the previous one.
    pub(crate) fn display(&self) -> String {
        let mut parts: Vec<&str> = vec![&self.name];
        for part in [self.region.as_deref(), Some(self.country.as_str())]
            .into_iter()
            .flatten()
        {
            if !part.is_empty() && parts.last() != Some(&part) {
                parts.push(part);
            }
        }
        parts.join(", ")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mention {
    /// The words as they appear in the text.
    pub matched: String,
    pub place: PlaceInfo,
}

/// Splits text into the word tokens names are matched on.
fn tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect()
}

fn key(text: &str) -> String {
    tokens(text).join(" ")
}

pub(crate) fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    6371.0088 * 2.0 * a.sqrt().min(1.0).asin()
}

impl Gazetteer {
    /// The embedded gazetteer, decoded on first use.
    pub(crate) fn embedded() -> Result<&'static Gazetteer> {
        static CELL: OnceLock<std::result::Result<Gazetteer, String>> = OnceLock::new();
        CELL.get_or_init(|| {
            let mut text = String::new();
            flate2::read::GzDecoder::new(DATA)
                .read_to_string(&mut text)
                .context("decompress embedded gazetteer")
                .and_then(|_| Gazetteer::parse(&text))
                .map_err(|e| format!("{e:#}"))
        })
        .as_ref()
        .map_err(|e| anyhow!("{e}"))
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let mut countries = Vec::new();
        let mut country_index: HashMap<String, u32> = HashMap::new();
        let mut regions: Vec<Region> = Vec::new();
        let mut region_index: HashMap<(u32, String), u32> = HashMap::new();
        let mut places = Vec::new();
        let mut country_of =
            |code: &str, countries: &mut Vec<Country>, name: Option<&str>| -> u32 {
                *country_index.entry(code.to_string()).or_insert_with(|| {
                    countries.push(Country {
                        code: code.into(),
                        name: name.unwrap_or(code).into(),
                    });
                    (countries.len() - 1) as u32
                })
            };
        let mut region_of = |country: u32, code: &str, regions: &mut Vec<Region>| -> u32 {
            *region_index
                .entry((country, code.to_string()))
                .or_insert_with(|| {
                    regions.push(Region {
                        country,
                        code: code.into(),
                        name: None,
                    });
                    (regions.len() - 1) as u32
                })
        };
        for (number, line) in text.lines().enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let bad = || anyhow!("gazetteer line {}: {line:?}", number + 1);
            match fields.as_slice() {
                ["C", code, name] => {
                    country_of(code, &mut countries, Some(name));
                }
                ["A", cc, code, name] => {
                    let country = country_of(cc, &mut countries, None);
                    let region = region_of(country, code, &mut regions);
                    regions[region as usize].name = Some((*name).into());
                }
                ["P", name, lat, lon, cc, admin1, population] => {
                    let country = country_of(cc, &mut countries, None);
                    places.push(Place {
                        name: (*name).into(),
                        lat_e3: lat.parse().map_err(|_| bad())?,
                        lon_e3: lon.parse().map_err(|_| bad())?,
                        region: region_of(country, admin1, &mut regions),
                        population: population.parse().map_err(|_| bad())?,
                    });
                }
                _ => return Err(bad()),
            }
        }

        let mut gazetteer = Self {
            countries,
            regions,
            places,
            names: HashMap::new(),
            upper: HashMap::new(),
        };
        gazetteer.build_name_index();
        Ok(gazetteer)
    }

    fn build_name_index(&mut self) {
        let stop: HashSet<&str> = STOPWORDS.iter().copied().collect();
        let usable = |k: &str| {
            !k.is_empty()
                && (k.contains(' ')
                    || (k.chars().count() >= 4 && !stop.contains(k.to_lowercase().as_str())))
        };
        let mut names: HashMap<String, Target> = HashMap::new();
        let mut population: HashMap<String, u32> = HashMap::new();
        for (i, place) in self.places.iter().enumerate() {
            if place.population < TEXT_MIN_POPULATION {
                continue;
            }
            let k = key(&place.name);
            if !usable(&k) || population.get(&k).is_some_and(|p| *p >= place.population) {
                continue;
            }
            population.insert(k.clone(), place.population);
            names.insert(k, Target::Place(i as u32));
        }
        // A country outranks a city of the same name (Armenia, Jamaica).
        for (i, country) in self.countries.iter().enumerate() {
            let k = key(&country.name);
            if usable(&k) {
                names.insert(k, Target::Country(i as u32));
            }
        }
        for (alias, target) in ALIASES {
            if let Some(target) = self.resolve_alias(*target) {
                names.insert((*alias).to_string(), target);
            }
        }
        self.upper = names.iter().map(|(k, t)| (k.to_uppercase(), *t)).collect();
        self.names = names;
    }

    fn resolve_alias(&self, alias: Alias) -> Option<Target> {
        let country = |cc: &str| self.countries.iter().position(|c| &*c.code == cc);
        match alias {
            Alias::Country(cc) => country(cc).map(|i| Target::Country(i as u32)),
            Alias::Region(cc, code) => {
                let c = country(cc)? as u32;
                self.regions
                    .iter()
                    .position(|r| r.country == c && &*r.code == code && r.name.is_some())
                    .map(|i| Target::Region(i as u32))
            }
            Alias::Place(name, cc) => {
                let c = country(cc)? as u32;
                self.places
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        &*p.name == name && self.regions[p.region as usize].country == c
                    })
                    .max_by_key(|(_, p)| p.population)
                    .map(|(i, _)| Target::Place(i as u32))
            }
        }
    }

    fn info(&self, target: Target) -> PlaceInfo {
        match target {
            Target::Place(i) => {
                let place = &self.places[i as usize];
                let region = &self.regions[place.region as usize];
                let country = &self.countries[region.country as usize];
                PlaceInfo {
                    kind: "city",
                    name: place.name.to_string(),
                    region: region.name.as_deref().map(str::to_string),
                    country: country.name.to_string(),
                    country_code: country.code.to_string(),
                    latitude: Some(f64::from(place.lat_e3) / 1e3),
                    longitude: Some(f64::from(place.lon_e3) / 1e3),
                    population: Some(place.population),
                }
            }
            Target::Region(i) => {
                let region = &self.regions[i as usize];
                let country = &self.countries[region.country as usize];
                PlaceInfo {
                    kind: "region",
                    name: region.name.as_deref().unwrap_or(&region.code).to_string(),
                    region: None,
                    country: country.name.to_string(),
                    country_code: country.code.to_string(),
                    latitude: None,
                    longitude: None,
                    population: None,
                }
            }
            Target::Country(i) => {
                let country = &self.countries[i as usize];
                PlaceInfo {
                    kind: "country",
                    name: country.name.to_string(),
                    region: None,
                    country: country.name.to_string(),
                    country_code: country.code.to_string(),
                    latitude: None,
                    longitude: None,
                    population: None,
                }
            }
        }
    }

    /// The nearest populated place to a coordinate and its distance in km.
    pub(crate) fn nearest(&self, latitude: f64, longitude: f64) -> Option<(PlaceInfo, f64)> {
        let scale = latitude.to_radians().cos();
        let (index, _) = self
            .places
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let dy = f64::from(p.lat_e3) / 1e3 - latitude;
                let mut dx = (f64::from(p.lon_e3) / 1e3 - longitude).abs();
                if dx > 180.0 {
                    dx = 360.0 - dx;
                }
                (i, dy * dy + (dx * scale) * (dx * scale))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))?;
        let info = self.info(Target::Place(index as u32));
        let distance = haversine_km(
            latitude,
            longitude,
            info.latitude.unwrap_or(latitude),
            info.longitude.unwrap_or(longitude),
        );
        Some((info, distance))
    }

    /// Place names mentioned in `text`, longest match first, each place once.
    pub(crate) fn mentions(&self, text: &str) -> Vec<Mention> {
        let words = tokens(text);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut i = 0;
        while i < words.len() {
            let mut matched = None;
            for n in (1..=MAX_NAME_TOKENS.min(words.len() - i)).rev() {
                let span = &words[i..i + n];
                let k = span.join(" ");
                let shouting = span.iter().all(|w| {
                    w.chars().any(char::is_alphabetic) && !w.chars().any(char::is_lowercase)
                }) && k.chars().filter(|c| c.is_alphabetic()).count() >= 4;
                let found = self
                    .names
                    .get(&k)
                    .or_else(|| if shouting { self.upper.get(&k) } else { None });
                if let Some(target) = found {
                    matched = Some((n, k, *target));
                    break;
                }
            }
            match matched {
                Some((n, k, target)) => {
                    if seen.insert(target) {
                        out.push(Mention {
                            matched: k,
                            place: self.info(target),
                        });
                    }
                    i += n;
                }
                None => i += 1,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# test gazetteer
C\tFR\tFrance
C\tGB\tUnited Kingdom
C\tUS\tUnited States
C\tAM\tArmenia
A\tFR\t11\tIle-de-France
A\tGB\tENG\tEngland
A\tUS\tCA\tCalifornia
A\tUS\tTX\tTexas
P\tParis\t48853\t2349\tFR\t11\t2138551
P\tParis\t33661\t-95556\tUS\tTX\t24782
P\tLondon\t51509\t-126\tGB\tENG\t8961989
P\tMountain View\t37386\t-122084\tUS\tCA\t80435
P\tSan Jose\t37339\t-121895\tUS\tCA\t1026908
P\tReading\t51456\t-971\tGB\tENG\t318014
P\tArmenia\t4534\t-75681\tCO\t\t315328
P\tVersailles\t48805\t2135\tFR\t11\t85416
";

    fn fixture() -> Gazetteer {
        Gazetteer::parse(FIXTURE).unwrap()
    }

    #[test]
    fn nearest_names_the_closest_town_with_region_and_country() {
        let g = fixture();
        let (place, km) = g.nearest(37.3861, -122.0839).unwrap();
        assert_eq!(place.display(), "Mountain View, California, United States");
        assert!(km < 1.0, "{km}");
        let (place, _) = g.nearest(48.80, 2.13).unwrap();
        assert_eq!(place.name, "Versailles");
    }

    #[test]
    fn mentions_prefer_longest_and_most_populous_names() {
        let g = fixture();
        let found = g.mentions("Flights from San Jose to Paris, then London.");
        let names: Vec<_> = found.iter().map(|m| m.place.display()).collect();
        assert_eq!(
            names,
            [
                "San Jose, California, United States",
                "Paris, Ile-de-France, France",
                "London, England, United Kingdom"
            ]
        );
    }

    #[test]
    fn mentions_skip_stopwords_lowercase_and_short_names() {
        let g = fixture();
        assert!(g.mentions("Reading the paris report in london").is_empty());
        let found = g.mentions("Welcome to LONDON and the USA");
        let codes: Vec<_> = found
            .iter()
            .map(|m| m.place.country_code.as_str())
            .collect();
        assert_eq!(codes, ["GB", "US"]);
        assert_eq!(found[0].matched, "LONDON");
    }

    #[test]
    fn countries_outrank_cities_and_aliases_resolve_regions() {
        let g = fixture();
        let found = g.mentions("Armenia and England, Paris and Paris again");
        let kinds: Vec<_> = found
            .iter()
            .map(|m| (m.place.kind, m.place.name.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("country", "Armenia"),
                ("region", "England"),
                ("city", "Paris")
            ]
        );
        assert_eq!(found[1].place.display(), "England, United Kingdom");
    }

    #[test]
    fn embedded_table_decodes_and_reverse_geocodes() {
        let g = Gazetteer::embedded().unwrap();
        assert!(g.places.len() > 100_000, "{}", g.places.len());
        let (place, km) = g.nearest(51.5007, -0.1246).unwrap();
        assert_eq!(place.country_code, "GB");
        assert!(km < 10.0, "{place:?} {km}");
        let found = g.mentions("Sunset over Paris");
        assert_eq!(found[0].place.country_code, "FR");
    }

    #[test]
    fn malformed_lines_are_rejected() {
        assert!(Gazetteer::parse("P\tParis\tnorth\t2\tFR\t11\t1\n").is_err());
        assert!(Gazetteer::parse("Z\tbad\n").is_err());
    }

    #[test]
    fn haversine_matches_known_distance() {
        let km = haversine_km(51.5074, -0.1278, 48.8566, 2.3522);
        assert!((km - 343.5).abs() < 2.0, "{km}");
    }
}
