//! Making a space, and what moving an entity to another space would take, as `nib space
//! new` and `nib move` say ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves)).
//! Which entities an app moves, and whether with their secret values, is the app's.

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_node::{Library, MovePreview};
use serde_json::{Value as Json, json};

use crate::{Failure, holding, wall_millis};

/// Makes a space of this user's, named as the request's `name` says, which they are the
/// admin of, for the devices of theirs that take new spaces and those the request names in
/// `also`: its ID.
///
/// # Errors
///
/// Where the name is empty, a device is no replica's ID, or the engine refuses.
pub fn create(library: &mut Library, request: &Json) -> Result<Json, Failure> {
	let name = request["name"].as_str().unwrap_or_default().trim();
	if name.is_empty() {
		return Err(Failure::Request("a space needs a name".to_owned()));
	}
	let also = holding::replicas_arg(request, "also")?;
	let space = library.create_space_on(name, &also, wall_millis(), &mut UnwrapErr(SysRng))?;
	Ok(json!({"id": space.to_string()}))
}

/// What moving an entity to another space would take, as the app shows it: how many
/// entities move, with what it holds, how many folders and lists of its space that stay
/// hold some of them, the members who will see them for the first time and those who will
/// stop, whether this user may purge all that stays behind, and whether it holds secret
/// values, or would be sealed there, which needs the secrets.
#[must_use]
pub fn move_preview_json(preview: &MovePreview) -> Json {
	json!({
		"count": preview.entities.len(),
		"staying": preview.staying.len(),
		"gaining": preview.gaining,
		"losing": preview.losing,
		"purges_all": preview.purges_all,
		"secrets": preview.secrets,
	})
}
