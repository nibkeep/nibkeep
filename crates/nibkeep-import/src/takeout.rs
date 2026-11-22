//! What Google Takeout writes of Google Photos: a JSON file beside each photo or video, with
//! when it was taken, where, its description and its name as uploaded, and a JSON file in
//! each album's folder, with the album's name and description. The JSON files' names follow
//! Takeout's habits, which [`metadata_for`] undoes.

use serde_json::Value as Json;

/// What a photo's or a video's JSON file says of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Metadata {
	/// Its name as uploaded, which its file's name may have shortened.
	pub(crate) title: Option<String>,
	/// Its description.
	pub(crate) description: Option<String>,
	/// When it was taken, in seconds since 1970, in UTC.
	pub(crate) taken: Option<i64>,
	/// When it reached Google Photos, in seconds since 1970.
	pub(crate) created: Option<i64>,
	/// Where it was taken, in degrees of latitude and longitude, unless it was 0, 0, which
	/// means nowhere.
	pub(crate) place: Option<(f64, f64)>,
}

/// What an album's JSON file says of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Album {
	/// Its name.
	pub(crate) title: String,
	/// Its description.
	pub(crate) description: Option<String>,
}

/// What a JSON file of Google Photos is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Described {
	/// A photo's or a video's.
	Media(Metadata),
	/// An album's.
	Album(Album),
	/// Something else, or not JSON at all.
	Other,
}

/// What the JSON file of `bytes` says.
pub(crate) fn describe(bytes: &[u8]) -> Described {
	let Ok(Json::Object(json)) = serde_json::from_slice::<Json>(bytes) else {
		return Described::Other;
	};
	let text = |key: &str| {
		json.get(key)
			.and_then(Json::as_str)
			.map(str::trim)
			.filter(|text| !text.is_empty())
			.map(str::to_owned)
	};
	let seconds = |key: &str| {
		json.get(key)
			.and_then(|at| at.get("timestamp"))
			.and_then(|at| match at {
				Json::String(text) => text.trim().parse::<i64>().ok(),
				Json::Number(number) => number.as_i64(),
				_ => None,
			})
	};
	if json.contains_key("photoTakenTime") || json.contains_key("creationTime") {
		let place = json.get("geoData").and_then(|geo| {
			let latitude = geo.get("latitude")?.as_f64()?;
			let longitude = geo.get("longitude")?.as_f64()?;
			let somewhere = (latitude != 0.0 || longitude != 0.0)
				&& latitude.abs() <= 90.0
				&& longitude.abs() <= 180.0;
			somewhere.then_some((latitude, longitude))
		});
		return Described::Media(Metadata {
			title: text("title"),
			description: text("description"),
			taken: seconds("photoTakenTime"),
			created: seconds("creationTime"),
			place,
		});
	}
	match text("title") {
		Some(title) => Described::Album(Album {
			title,
			description: text("description"),
		}),
		None => Described::Other,
	}
}

/// The marks that Google Photos puts on the name of an edited copy, before its extension, in
/// the languages it is used in most: the edited copy shares its original's JSON file.
const EDITED: [&str; 8] = [
	"-edited",
	"-bearbeitet",
	"-modifié",
	"-modificato",
	"-editado",
	"-bewerkt",
	"-redigerad",
	"-edytowane",
];

/// How long a JSON file's name, without `.json`, is at least when Takeout shortened it: its
/// names are cut at 51 characters, `.json` included, so a shortened one is at least this
/// long, and a shorter one that starts as the photo's name is another photo's.
const SHORTENED: usize = 40;

/// A photo's or a video's file name, as Takeout writes it: its stem, without a copy's number
/// or an edited mark, the copy's number, whether it is an edited copy, and its extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MediaName {
	pub(crate) stem: String,
	pub(crate) copy: Option<u32>,
	pub(crate) edited: bool,
	pub(crate) extension: String,
}

impl MediaName {
	/// The parts of the file name `name`.
	pub(crate) fn of(name: &str) -> Self {
		let (mut stem, extension) = match name.rfind('.') {
			Some(dot) if dot > 0 => (name[..dot].to_owned(), name[dot..].to_owned()),
			_ => (name.to_owned(), String::new()),
		};
		let mut copy = None;
		if let Some(open) = stem.rfind('(')
			&& stem.ends_with(')')
			&& let Ok(number) = stem[open + 1..stem.len() - 1].parse::<u32>()
		{
			copy = Some(number);
			stem.truncate(open);
		}
		let mut edited = false;
		for mark in EDITED {
			if stem.to_lowercase().ends_with(mark) && stem.len() > mark.len() {
				stem.truncate(stem.len() - mark.len());
				edited = true;
				break;
			}
		}
		Self {
			stem,
			copy,
			edited,
			extension,
		}
	}

	/// The name it had as uploaded: its stem and extension.
	pub(crate) fn original(&self) -> String {
		format!("{}{}", self.stem, self.extension)
	}
}

/// The names that a photo's or a video's JSON file has exactly, by Takeout's habits, as
/// [`metadata_for`] looks for them first: so that a folder of many photos finds each by name.
pub(crate) fn exact_names(name: &str) -> Vec<String> {
	let media = MediaName::of(name);
	let original = media.original();
	let copy = media.copy.map_or_default(|number| format!("({number})"));
	let mut names = vec![
		format!("{original}{copy}.json"),
		format!("{original}.supplemental-metadata{copy}.json"),
	];
	if !media.edited {
		names.push(format!("{name}.json"));
	}
	names
}

/// Of the JSON files in a photo's or a video's folder, `jsons`, each by its file name and
/// the title it says, the one of the file named `name`: by Takeout's habits with names, cut
/// short, the copy's number after the extension, an edited copy sharing its original's, and
/// the later `.supplemental-metadata` before `.json`; else the one whose title is its name,
/// if one alone is.
pub(crate) fn metadata_for(name: &str, jsons: &[(&str, Option<&str>)]) -> Option<usize> {
	let media = MediaName::of(name);
	let original = media.original();
	let copy = media.copy.map_or_default(|number| format!("({number})"));
	let stem_of = |json: &str| -> Option<String> {
		let (stem, extension) = json.rsplit_once('.')?;
		extension
			.eq_ignore_ascii_case("json")
			.then(|| stem.to_owned())
	};
	let mut shortened = None;
	for (index, (json, _)) in jsons.iter().enumerate() {
		let Some(stem) = stem_of(json) else {
			continue;
		};
		for full in [
			original.clone(),
			format!("{original}.supplemental-metadata"),
		] {
			// The copy's number after the whole name, or, from older archives, the file's own
			// name before `.json`.
			if stem == format!("{full}{copy}") || (!media.edited && stem == name) {
				return Some(index);
			}
			if let Some(cut) = stem.strip_suffix(copy.as_str())
				&& cut.chars().count() >= SHORTENED
				&& full.starts_with(cut)
				&& shortened.is_none()
			{
				shortened = Some(index);
			}
		}
	}
	if shortened.is_some() {
		return shortened;
	}
	if media.copy.is_some() || media.edited {
		return None;
	}
	// By the title, the name as uploaded, which the file's own name may have cut short.
	let titled: Vec<usize> = jsons
		.iter()
		.enumerate()
		.filter(|(_, (_, title))| {
			title.is_some_and(|title| {
				title == original
					|| (media.stem.chars().count() >= SHORTENED
						&& title.starts_with(&media.stem)
						&& title.ends_with(&media.extension))
			})
		})
		.map(|(index, _)| index)
		.collect();
	match titled.as_slice() {
		[one] => Some(*one),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn found(name: &str, jsons: &[(&str, Option<&str>)]) -> Option<String> {
		metadata_for(name, jsons).map(|index| jsons[index].0.to_owned())
	}

	#[test]
	fn a_photo_finds_its_json_by_takeouts_habits() {
		let jsons = [
			("IMG_1234.jpg.json", Some("IMG_1234.jpg")),
			("IMG_1234.jpg(1).json", Some("IMG_1234.jpg")),
			(
				"IMG_5678.HEIC.supplemental-metadata.json",
				Some("IMG_5678.HEIC"),
			),
			(
				"PXL_20230825_142745123.NIGHT_with_a_long_n.json",
				Some("PXL_20230825_142745123.NIGHT_with_a_long_name.jpg"),
			),
			(
				"PXL_20240101_101010101.MP.jpg.supplemental-me.json",
				Some("PXL_20240101_101010101.MP.jpg"),
			),
			("metadata.json", None),
		];
		assert_eq!(
			found("IMG_1234.jpg", &jsons).as_deref(),
			Some("IMG_1234.jpg.json")
		);
		assert_eq!(
			found("IMG_1234(1).jpg", &jsons).as_deref(),
			Some("IMG_1234.jpg(1).json")
		);
		assert_eq!(
			found("IMG_1234-edited.jpg", &jsons).as_deref(),
			Some("IMG_1234.jpg.json")
		);
		assert_eq!(
			found("IMG_5678.HEIC", &jsons).as_deref(),
			Some("IMG_5678.HEIC.supplemental-metadata.json")
		);
		assert_eq!(
			found("PXL_20230825_142745123.NIGHT_with_a_long_name.jpg", &jsons).as_deref(),
			Some("PXL_20230825_142745123.NIGHT_with_a_long_n.json")
		);
		assert_eq!(
			found("PXL_20240101_101010101.MP.jpg", &jsons).as_deref(),
			Some("PXL_20240101_101010101.MP.jpg.supplemental-me.json")
		);
		// No JSON of its own: nothing, rather than another photo's.
		assert_eq!(found("IMG_123.jpg", &jsons), None);
		assert_eq!(found("IMG_1234(2).jpg", &jsons), None);
	}

	#[test]
	fn a_photo_whose_name_takeout_cut_short_finds_its_json_by_title() {
		let jsons = [(
			"a weekend at the lake with the whole family and.json",
			Some("a weekend at the lake with the whole family and the dog.jpg"),
		)];
		assert_eq!(
			metadata_for(
				"a weekend at the lake with the whole family and the.jpg",
				&jsons
			),
			Some(0)
		);
		let renamed = [("renamed.json", Some("IMG_9.jpg"))];
		assert_eq!(metadata_for("IMG_9.jpg", &renamed), Some(0));
	}

	#[test]
	fn a_json_tells_of_a_photo_or_an_album() {
		let photo = br#"{"title":"IMG_1234.jpg","description":"Grandma's kitchen",
			"photoTakenTime":{"timestamp":"1577208600"},"creationTime":{"timestamp":"1577300000"},
			"geoData":{"latitude":59.33,"longitude":18.07,"altitude":0.0}}"#;
		assert_eq!(
			describe(photo),
			Described::Media(Metadata {
				title: Some("IMG_1234.jpg".to_owned()),
				description: Some("Grandma's kitchen".to_owned()),
				taken: Some(1_577_208_600),
				created: Some(1_577_300_000),
				place: Some((59.33, 18.07)),
			})
		);
		let nowhere = br#"{"title":"a.jpg","description":"","photoTakenTime":{"timestamp":"1"},
			"geoData":{"latitude":0.0,"longitude":0.0}}"#;
		let Described::Media(nowhere) = describe(nowhere) else {
			panic!("a photo's");
		};
		assert_eq!((nowhere.place, nowhere.description), (None, None));
		let album =
			br#"{"title":"Christmas 2019","description":"At Grandma's","access":"protected"}"#;
		assert_eq!(
			describe(album),
			Described::Album(Album {
				title: "Christmas 2019".to_owned(),
				description: Some("At Grandma's".to_owned()),
			})
		);
		assert_eq!(describe(b"not json"), Described::Other);
		assert_eq!(describe(br#"{"print":"subscriptions"}"#), Described::Other);
	}
}
