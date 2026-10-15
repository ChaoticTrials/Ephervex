//! Favorites, groups and icons around instances and servers (ADR 0048).
//!
//! This is presentation state. It lives in one launcher-level file beside the
//! index and is never part of a host's own metadata, so changing it cannot
//! alter an instance's identity, paths, links, snapshots or timestamps, and it
//! does not take a host claim: it works while a host is running or moving.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use ephervex_i18n::Arg;
use ephervex_model::{GroupId, InstanceId, ServerId};
use ephervex_storage::{IconId, Organization};
use image::{ImageFormat, imageops::FilterType};
use sha2::{Digest as _, Sha256};
use tracing::warn;

use crate::bounded_image::{ICON_SOURCE_BOUNDS, ImageRefusal, decode_bounded, encode_png};
use crate::error::{CoreError, CoreResult};
use crate::instance::validated_entity_name;
use crate::{App, InstanceFacts, InstanceSort, VersionOrder};

/// The largest icon file read, before anything is decoded.
pub const MAX_ICON_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// The side an imported icon is scaled down to fit within.
pub const ICON_MAX_SIDE: u32 = 256;

/// How many color slots groups are spread over.
pub const GROUP_TONE_COUNT: u8 = 6;

/// A user-defined group label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceGroup {
    pub id: GroupId,
    pub name: String,
    /// The group's color slot, below [`GROUP_TONE_COUNT`].
    pub tone: u8,
}

/// Why a picked icon file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IconRefusal {
    /// It could not be read.
    Unreadable,
    /// It is not a regular file.
    NotAFile,
    /// It is larger than [`MAX_ICON_SOURCE_BYTES`].
    TooLarge,
    /// It is not a PNG or JPEG image.
    UnsupportedFormat,
    /// It is damaged, or not an image at all.
    NotAnImage,
    /// Its dimensions are beyond what an icon can be.
    TooManyPixels,
}

impl IconRefusal {
    /// The `ephervex-i18n` message id explaining this to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Unreadable => "error-icon-unreadable",
            Self::NotAFile => "error-icon-not-a-file",
            Self::TooLarge => "error-icon-too-large",
            Self::UnsupportedFormat => "error-icon-unsupported-format",
            Self::NotAnImage => "error-icon-not-an-image",
            Self::TooManyPixels => "error-icon-too-many-pixels",
        }
    }

    pub(crate) fn message_args(self) -> Vec<(&'static str, Arg<'static>)> {
        match self {
            Self::TooLarge => vec![(
                "limit_mib",
                Arg::from((MAX_ICON_SOURCE_BYTES / (1024 * 1024)).to_string()),
            )],
            Self::TooManyPixels => vec![
                ("limit", Arg::from(ICON_SOURCE_BOUNDS.max_side.to_string())),
                (
                    "megapixels",
                    Arg::from((ICON_SOURCE_BOUNDS.max_pixels / (1024 * 1024)).to_string()),
                ),
            ],
            _ => Vec::new(),
        }
    }
}

impl std::fmt::Display for IconRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unreadable => "it could not be read",
            Self::NotAFile => "it is not a file",
            Self::TooLarge => "it is too large",
            Self::UnsupportedFormat => "only PNG and JPEG images are accepted",
            Self::NotAnImage => "it is not a readable image",
            Self::TooManyPixels => "its dimensions are too large",
        })
    }
}

/// What the launcher remembers about how hosts are presented, read once.
#[derive(Debug, Clone)]
pub struct OrganizationView {
    organization: Organization,
    icons_dir: PathBuf,
}

impl OrganizationView {
    #[must_use]
    pub fn is_favorite_client(&self, client: InstanceId) -> bool {
        self.organization.favorite_clients.contains(&client)
    }

    #[must_use]
    pub fn is_favorite_server(&self, server: ServerId) -> bool {
        self.organization.favorite_servers.contains(&server)
    }

    /// The group `client` is in, or `None` when it is ungrouped.
    #[must_use]
    pub fn group_of(&self, client: InstanceId) -> Option<InstanceGroup> {
        self.organization
            .group_of(client)
            .map(|(id, name)| InstanceGroup {
                id,
                name: name.to_owned(),
                tone: tone_of(&self.organization, id),
            })
    }

    /// Every group, in name order without regard to case.
    #[must_use]
    pub fn groups(&self) -> Vec<InstanceGroup> {
        let mut groups: Vec<InstanceGroup> = self
            .organization
            .groups
            .iter()
            .map(|(id, name)| InstanceGroup {
                id: *id,
                name: name.clone(),
                tone: tone_of(&self.organization, *id),
            })
            .collect();
        groups.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.cmp(&b.id))
        });
        groups
    }

    /// The group with this name, compared without regard to case.
    #[must_use]
    pub fn group_named(&self, name: &str) -> Option<InstanceGroup> {
        let wanted = name.trim().to_lowercase();
        self.groups()
            .into_iter()
            .find(|group| group.name.to_lowercase() == wanted)
    }

    /// The stored icon file for a client, when one is set and still on disk.
    #[must_use]
    pub fn client_icon(&self, client: InstanceId) -> Option<PathBuf> {
        self.existing_icon(&self.organization.client_icon(client)?)
    }

    /// The stored icon file for a server, when one is set and still on disk.
    #[must_use]
    pub fn server_icon(&self, server: ServerId) -> Option<PathBuf> {
        self.existing_icon(&self.organization.server_icon(server)?)
    }

    /// Whether a client names an icon, whether or not its file is still there.
    #[must_use]
    pub fn client_has_icon(&self, client: InstanceId) -> bool {
        self.organization.client_icon(client).is_some()
    }

    fn existing_icon(&self, icon: &IconId) -> Option<PathBuf> {
        let path = self.icons_dir.join(icon.file_name());
        path.is_file().then_some(path)
    }
}

/// Files this process moved aside because they could not be used, since it
/// last asked, and whether the previous copy replaced each. Reading or
/// changing favorites, groups and icons can do the moving, so a frontend asks
/// after a command or a refresh and tells the user.
#[must_use]
pub fn take_organization_notices() -> Vec<ephervex_storage::SetAsideNotice> {
    ephervex_storage::take_set_aside_notices()
}

/// An icon ready to be stored: scaled, re-encoded and named by its content.
struct PreparedIcon {
    id: IconId,
    png: Vec<u8>,
}

fn refused(path: &Path, reason: IconRefusal) -> CoreError {
    CoreError::IconRefused {
        path: path.to_path_buf(),
        reason,
    }
}

/// Reads an untrusted image file and turns it into a small PNG.
fn prepare_icon(source: &Path) -> CoreResult<PreparedIcon> {
    let metadata =
        std::fs::metadata(source).map_err(|_| refused(source, IconRefusal::Unreadable))?;
    if !metadata.is_file() {
        return Err(refused(source, IconRefusal::NotAFile));
    }
    if metadata.len() > MAX_ICON_SOURCE_BYTES {
        return Err(refused(source, IconRefusal::TooLarge));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(source)
        .and_then(|file| file.take(MAX_ICON_SOURCE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| refused(source, IconRefusal::Unreadable))?;
    if bytes.len() as u64 > MAX_ICON_SOURCE_BYTES {
        return Err(refused(source, IconRefusal::TooLarge));
    }

    let map = |refusal: ImageRefusal| {
        refused(
            source,
            match refusal {
                ImageRefusal::UnsupportedFormat => IconRefusal::UnsupportedFormat,
                ImageRefusal::TooLarge { .. } | ImageRefusal::TooManyPixels { .. } => {
                    IconRefusal::TooManyPixels
                }
                ImageRefusal::Unusable(_) => IconRefusal::NotAnImage,
            },
        )
    };
    let decoded = decode_bounded(
        || Ok(std::io::Cursor::new(bytes.as_slice())),
        Some(&[ImageFormat::Png, ImageFormat::Jpeg]),
        ICON_SOURCE_BOUNDS,
    )
    .map_err(map)?;
    let scaled = if decoded.width() > ICON_MAX_SIDE || decoded.height() > ICON_MAX_SIDE {
        decoded.resize(ICON_MAX_SIDE, ICON_MAX_SIDE, FilterType::Lanczos3)
    } else {
        decoded
    };
    let png = encode_png(&scaled).map_err(map)?;
    let digest: [u8; 32] = Sha256::digest(&png).into();
    Ok(PreparedIcon {
        id: IconId::from_digest(&digest),
        png,
    })
}

fn store_icon(app: &App, icon: &PreparedIcon) -> CoreResult<()> {
    Ok(Organization::store_icon(app.paths(), &icon.id, &icon.png)?)
}

impl App {
    /// Reads favorites, groups and icons.
    pub fn organization(&self) -> CoreResult<OrganizationView> {
        let loaded = Organization::load(self.paths())?;
        Ok(OrganizationView {
            organization: loaded.organization,
            icons_dir: self.paths().icons_dir(),
        })
    }

    /// [`Self::organization`], or an empty one when it cannot be read.
    ///
    /// For a view that must keep working: favorites, groups and icons are
    /// presentation, so the list is still shown without them.
    #[must_use]
    pub fn organization_or_empty(&self) -> OrganizationView {
        self.organization().unwrap_or_else(|error| {
            warn!(%error, "favorites, groups and icons could not be read");
            OrganizationView {
                organization: Organization::default(),
                icons_dir: self.paths().icons_dir(),
            }
        })
    }

    /// Changes the organization under its lock, dropping entries for hosts
    /// that no longer exist when the index can be read.
    fn change_organization<T>(
        &self,
        change: impl FnOnce(&mut Organization) -> CoreResult<T>,
    ) -> CoreResult<T> {
        Organization::update(self.paths(), |organization| {
            let result = change(organization)?;
            self.forget_stale_organization(organization);
            Ok::<_, CoreError>(result)
        })
    }

    /// Never prunes from a listing that failed: an unreadable index says
    /// nothing about which hosts exist.
    fn forget_stale_organization(&self, organization: &mut Organization) {
        let (Ok(clients), Ok(servers)) = (self.list_clients(), self.list_servers()) else {
            return;
        };
        let clients: BTreeSet<InstanceId> = clients.into_iter().map(|record| record.id).collect();
        let servers: BTreeSet<ServerId> = servers.into_iter().map(|record| record.id).collect();
        organization.retain_hosts(&clients, &servers);
    }

    /// Marks or unmarks an instance as a favorite.
    pub fn set_client_favorite(&self, client: InstanceId, favorite: bool) -> CoreResult<()> {
        self.client(client)?;
        self.change_organization(|organization| {
            if favorite {
                organization.favorite_clients.insert(client);
            } else {
                organization.favorite_clients.remove(&client);
            }
            Ok(())
        })
    }

    /// Marks or unmarks a server as a favorite.
    pub fn set_server_favorite(&self, server: ServerId, favorite: bool) -> CoreResult<()> {
        self.server(server)?;
        self.change_organization(|organization| {
            if favorite {
                organization.favorite_servers.insert(server);
            } else {
                organization.favorite_servers.remove(&server);
            }
            Ok(())
        })
    }

    /// Creates a group. Names are unique without regard to case.
    pub fn create_group(&self, name: &str) -> CoreResult<InstanceGroup> {
        let name = validated_entity_name(name)?;
        self.change_organization(|organization| {
            ensure_group_name_free(organization, &name, None)?;
            let id = GroupId::new();
            let tone = least_used_tone(organization);
            organization.groups.insert(id, name.clone());
            organization.group_tones.insert(id, tone);
            Ok(InstanceGroup { id, name, tone })
        })
    }

    /// Renames a group. Its members keep it.
    pub fn rename_group(&self, group: GroupId, name: &str) -> CoreResult<InstanceGroup> {
        let name = validated_entity_name(name)?;
        self.change_organization(|organization| {
            if !organization.groups.contains_key(&group) {
                return Err(CoreError::GroupNotFound {
                    group: group.to_string(),
                });
            }
            ensure_group_name_free(organization, &name, Some(group))?;
            organization.groups.insert(group, name.clone());
            Ok(InstanceGroup {
                id: group,
                name,
                tone: tone_of(organization, group),
            })
        })
    }

    /// Deletes a group. Its members become ungrouped.
    pub fn delete_group(&self, group: GroupId) -> CoreResult<()> {
        self.change_organization(|organization| {
            if organization.groups.remove(&group).is_none() {
                return Err(CoreError::GroupNotFound {
                    group: group.to_string(),
                });
            }
            organization.group_tones.remove(&group);
            organization
                .client_groups
                .retain(|_, assigned| *assigned != group);
            Ok(())
        })
    }

    /// Puts an instance in a group, or leaves it ungrouped with `None`.
    pub fn set_client_group(&self, client: InstanceId, group: Option<GroupId>) -> CoreResult<()> {
        self.client(client)?;
        self.change_organization(|organization| {
            match group {
                Some(group) if organization.groups.contains_key(&group) => {
                    organization.client_groups.insert(client, group);
                }
                Some(group) => {
                    return Err(CoreError::GroupNotFound {
                        group: group.to_string(),
                    });
                }
                None => {
                    organization.client_groups.remove(&client);
                }
            }
            Ok(())
        })
    }

    /// Sets an instance's icon from a local PNG or JPEG file.
    ///
    /// The file is untrusted: it is size-limited, decoded within bounds,
    /// scaled down and re-encoded, and only the result is kept, by content.
    pub fn set_client_icon(&self, client: InstanceId, source: &Path) -> CoreResult<()> {
        self.client(client)?;
        let icon = prepare_icon(source)?;
        self.change_organization(|organization| {
            store_icon(self, &icon)?;
            organization
                .client_icons
                .insert(client, icon.id.to_string());
            Ok(())
        })
    }

    /// Removes an instance's icon.
    pub fn clear_client_icon(&self, client: InstanceId) -> CoreResult<()> {
        self.client(client)?;
        self.change_organization(|organization| {
            organization.client_icons.remove(&client);
            Ok(())
        })
    }

    /// Sets a server's icon. See [`Self::set_client_icon`].
    pub fn set_server_icon(&self, server: ServerId, source: &Path) -> CoreResult<()> {
        self.server(server)?;
        let icon = prepare_icon(source)?;
        self.change_organization(|organization| {
            store_icon(self, &icon)?;
            organization
                .server_icons
                .insert(server, icon.id.to_string());
            Ok(())
        })
    }

    /// Removes a server's icon.
    pub fn clear_server_icon(&self, server: ServerId) -> CoreResult<()> {
        self.server(server)?;
        self.change_organization(|organization| {
            organization.server_icons.remove(&server);
            Ok(())
        })
    }

    /// Gives a duplicated instance its original's group and icon, never its
    /// favorite. Presentation only, so a failure is reported and not raised.
    pub(crate) fn inherit_organization(&self, original: InstanceId, copy: InstanceId) {
        let inherited = self.change_organization(|organization| {
            if let Some(group) = organization.client_groups.get(&original).copied() {
                organization.client_groups.insert(copy, group);
            }
            if let Some(icon) = organization.client_icons.get(&original).cloned() {
                organization.client_icons.insert(copy, icon);
            }
            Ok(())
        });
        if let Err(error) = inherited {
            warn!(%error, "a duplicated instance did not inherit its group and icon");
        }
    }

    /// The instance list's order, falling back to the default for a recorded
    /// name this build does not know.
    #[must_use]
    pub fn instance_sort(&self) -> InstanceSort {
        InstanceSort::from_name(&self.config().display.instance_sort).unwrap_or_default()
    }

    /// Records the instance list's order.
    pub fn set_instance_sort(&mut self, sort: InstanceSort) -> CoreResult<()> {
        self.mutate_config(|config| sort.as_str().clone_into(&mut config.display.instance_sort))
    }

    /// How Minecraft versions are ranked, from the cached manifest alone.
    pub async fn minecraft_version_order(&self) -> VersionOrder {
        let layout = ephervex_download::CacheLayout::new(self.paths().cache_dir());
        match ephervex_minecraft::cached_version_manifest(&layout).await {
            Some(manifest) => VersionOrder::from_newest_first(
                manifest.versions.into_iter().map(|record| record.id),
            ),
            None => VersionOrder::default(),
        }
    }

    /// The facts about every instance that search, filters and ordering use.
    pub fn instance_facts(&self) -> CoreResult<Vec<InstanceFacts>> {
        let organization = self.organization()?;
        let linking = self.linking_states()?;
        Ok(self
            .list_clients()?
            .iter()
            .map(|record| {
                InstanceFacts::from_record(
                    record,
                    organization.is_favorite_client(record.id),
                    organization.group_of(record.id),
                    linking
                        .get(&record.id)
                        .copied()
                        .unwrap_or(crate::LinkingState::None),
                    ephervex_storage::running_state(&record.root).is_some(),
                )
            })
            .collect())
    }
}

/// A group's color slot. One the file does not hold, or holds out of range,
/// is derived from the id, so it is still the same every time.
fn tone_of(organization: &Organization, group: GroupId) -> u8 {
    organization
        .group_tones
        .get(&group)
        .copied()
        .filter(|tone| *tone < GROUP_TONE_COUNT)
        .unwrap_or_else(|| group.as_uuid().as_bytes()[0] % GROUP_TONE_COUNT)
}

/// The slot fewest groups use, the lowest on a tie, so a handful of groups
/// all look different.
fn least_used_tone(organization: &Organization) -> u8 {
    let mut uses = [0_usize; GROUP_TONE_COUNT as usize];
    for group in organization.groups.keys() {
        uses[usize::from(tone_of(organization, *group))] += 1;
    }
    (0..GROUP_TONE_COUNT)
        .min_by_key(|tone| uses[usize::from(*tone)])
        .unwrap_or(0)
}

fn ensure_group_name_free(
    organization: &Organization,
    name: &str,
    except: Option<GroupId>,
) -> CoreResult<()> {
    let wanted = name.to_lowercase();
    let taken = organization
        .groups
        .iter()
        .any(|(id, existing)| Some(*id) != except && existing.to_lowercase() == wanted);
    if taken {
        return Err(CoreError::GroupNameTaken {
            name: name.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use ephervex_download::OperationReporter;
    use ephervex_model::{LoaderSelection, StorageClass};
    use ephervex_storage::{DuplicateClient, NewClient, NewServer, Paths};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::promotion::PromotionRequest;
    use crate::snapshot::{SnapshotOrigin, SnapshotRestoreOptions};
    use crate::{DiagnosticScope, InstalledHost};

    struct World {
        base: tempfile::TempDir,
        app: App,
    }

    fn world() -> World {
        let base = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(base.path().join("roots"))).expect("open");
        World { base, app }
    }

    impl World {
        fn client(&self, name: &str) -> InstanceId {
            self.app
                .create_client_for_test(NewClient::new(name, "1.20.1", LoaderSelection::vanilla()))
                .expect("client")
                .id
        }

        fn server(&self, name: &str) -> ServerId {
            let staging = self.base.path().join(format!("staged-{name}"));
            std::fs::create_dir_all(staging.join("server")).expect("staging");
            let record = self
                .app
                .registry_for_services()
                .prepare_server(NewServer::new(name, "1.20.1", LoaderSelection::vanilla()))
                .expect("prepare");
            self.app
                .registry_for_services()
                .publish_staged_server(&record, &staging)
                .expect("publish");
            record.id
        }

        fn reopened(&self) -> App {
            App::open_at(self.app.paths().clone()).expect("reopen")
        }

        fn image(&self, name: &str, width: u32, height: u32, format: ImageFormat) -> PathBuf {
            let path = self.base.path().join(name);
            let image =
                image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]));
            image::DynamicImage::ImageRgba8(image)
                .save_with_format(&path, format)
                .expect("write image");
            path
        }

        fn icon_files(&self) -> usize {
            std::fs::read_dir(self.app.paths().icons_dir()).map_or(0, Iterator::count)
        }
    }

    fn refusal(error: CoreError) -> IconRefusal {
        match error {
            CoreError::IconRefused { reason, .. } => reason,
            other => panic!("expected an icon refusal, got {other}"),
        }
    }

    #[test]
    fn favorites_persist_and_never_touch_the_instance_record() {
        let world = world();
        let id = world.client("repro");
        let before = world.app.client(id).expect("client");
        let metadata = ephervex_storage::metadata::metadata_dir(&before.root).join("instance.json");
        let metadata_before = std::fs::read(&metadata).expect("metadata");

        world.app.set_client_favorite(id, true).expect("favorite");

        assert!(
            world
                .reopened()
                .organization()
                .unwrap()
                .is_favorite_client(id)
        );
        assert_eq!(world.app.client(id).expect("client"), before);
        assert_eq!(std::fs::read(&metadata).expect("metadata"), metadata_before);

        world
            .app
            .set_client_favorite(id, false)
            .expect("unfavorite");
        assert!(!world.app.organization().unwrap().is_favorite_client(id));
    }

    #[test]
    fn a_favorite_is_refused_for_an_instance_that_does_not_exist() {
        let world = world();
        assert!(
            world
                .app
                .set_client_favorite(InstanceId::new(), true)
                .is_err()
        );
        assert!(
            world
                .app
                .set_server_favorite(ServerId::new(), true)
                .is_err()
        );
    }

    #[test]
    fn servers_can_be_favorited_too() {
        let world = world();
        let server = world.server("repro server");
        world
            .app
            .set_server_favorite(server, true)
            .expect("favorite");
        assert!(
            world
                .reopened()
                .organization()
                .unwrap()
                .is_favorite_server(server)
        );
    }

    #[test]
    fn groups_are_created_renamed_and_deleted_with_members_ungrouped() {
        let world = world();
        let id = world.client("repro");
        let group = world.app.create_group("  Regression  ").expect("create");
        assert_eq!(group.name, "Regression");
        world
            .app
            .set_client_group(id, Some(group.id))
            .expect("assign");

        let renamed = world.app.rename_group(group.id, "Bisect").expect("rename");
        assert_eq!(renamed.id, group.id);
        let view = world.reopened().organization().unwrap();
        assert_eq!(view.group_of(id), Some(renamed));

        world.app.delete_group(group.id).expect("delete");
        let view = world.app.organization().unwrap();
        assert_eq!(view.group_of(id), None);
        assert!(view.groups().is_empty());
    }

    #[test]
    fn group_names_are_unique_without_regard_to_case_and_validated() {
        let world = world();
        let first = world.app.create_group("Regression").expect("create");
        let taken = world.app.create_group("REGRESSION").expect_err("taken");
        assert_eq!(taken.message_id(), "error-group-name-taken");
        let other = world.app.create_group("Other").expect("create");
        assert!(world.app.rename_group(other.id, "regression").is_err());
        assert!(world.app.rename_group(first.id, "REGRESSION").is_ok());
        assert_eq!(
            world
                .app
                .create_group("   ")
                .expect_err("blank")
                .message_id(),
            "error-name-empty"
        );
        assert!(world.app.create_group("bad\nname").is_err());
    }

    #[test]
    fn groups_list_in_case_insensitive_name_order_and_empty_ones_stay() {
        let world = world();
        for name in ["beta", "Alpha", "gamma"] {
            world.app.create_group(name).expect("create");
        }
        let names: Vec<String> = world
            .reopened()
            .organization()
            .unwrap()
            .groups()
            .into_iter()
            .map(|group| group.name)
            .collect();
        assert_eq!(names, ["Alpha", "beta", "gamma"]);
    }

    #[test]
    fn assigning_a_missing_group_or_instance_is_refused() {
        let world = world();
        let id = world.client("repro");
        assert_eq!(
            world
                .app
                .set_client_group(id, Some(GroupId::new()))
                .expect_err("no group")
                .message_id(),
            "error-group-not-found"
        );
        let group = world.app.create_group("G").expect("create");
        assert!(
            world
                .app
                .set_client_group(InstanceId::new(), Some(group.id))
                .is_err()
        );
    }

    #[test]
    fn group_lookup_by_name_ignores_case() {
        let world = world();
        let group = world.app.create_group("Regression").expect("create");
        assert_eq!(
            world.app.organization().unwrap().group_named("regRESSION"),
            Some(group)
        );
        assert_eq!(world.app.organization().unwrap().group_named("none"), None);
    }

    #[test]
    fn deleting_an_instance_drops_its_organization_on_the_next_write() {
        let world = world();
        let gone = world.client("gone");
        let kept = world.client("kept");
        world.app.set_client_favorite(gone, true).expect("favorite");
        world
            .app
            .delete_client(gone, ephervex_storage::FileDisposition::Remove)
            .expect("delete");

        // The next write drops what named the deleted instance.
        world.app.set_client_favorite(kept, true).expect("favorite");
        let view = world.app.organization().unwrap();
        assert!(!view.is_favorite_client(gone));
        assert!(view.is_favorite_client(kept));
        assert!(!view.organization.favorite_clients.contains(&gone));
    }

    #[test]
    fn nothing_is_pruned_while_the_index_cannot_be_read() {
        let world = world();
        let id = world.client("repro");
        world.app.set_client_favorite(id, true).expect("favorite");
        // A stand-in for an unreadable index: the pruning step is skipped
        // when either listing fails, so the favorite of a host the failed
        // listing could not name is kept.
        let mut organization = world.app.organization().unwrap().organization.clone();
        let stale = InstanceId::new();
        organization.favorite_clients.insert(stale);
        let index = world.app.registry().index().connection();
        index
            .execute_batch("ALTER TABLE instances RENAME TO instances_hidden;")
            .expect("hide");
        world.app.forget_stale_organization(&mut organization);
        index
            .execute_batch("ALTER TABLE instances_hidden RENAME TO instances;")
            .expect("restore");
        assert!(organization.favorite_clients.contains(&stale));
        assert!(organization.favorite_clients.contains(&id));

        world.app.forget_stale_organization(&mut organization);
        assert!(!organization.favorite_clients.contains(&stale));
        assert!(organization.favorite_clients.contains(&id));
    }

    #[test]
    fn organization_changes_leave_identity_paths_links_and_timestamps_alone() {
        let world = world();
        let id = world.client("repro");
        let server = world.server("repro server");
        let before = world.app.client(id).expect("client");
        let links = world.app.linked_server_ids(id).expect("links");
        let pairs = world.app.linked_pairs().expect("pairs");
        let png = world.image("icon.png", 32, 32, ImageFormat::Png);

        let group = world.app.create_group("G").expect("group");
        world
            .app
            .set_client_group(id, Some(group.id))
            .expect("assign");
        world.app.set_client_favorite(id, true).expect("favorite");
        world.app.set_client_icon(id, &png).expect("icon");
        world
            .app
            .set_server_favorite(server, true)
            .expect("favorite");
        world.app.rename_group(group.id, "H").expect("rename");
        world.app.clear_client_icon(id).expect("clear");

        assert_eq!(world.app.client(id).expect("client"), before);
        assert_eq!(world.app.linked_server_ids(id).expect("links"), links);
        assert_eq!(world.app.linked_pairs().expect("pairs"), pairs);
    }

    #[test]
    fn a_restore_does_not_roll_organization_back() {
        let world = world();
        let id = world.client("repro");
        let host = InstalledHost::Instance(id);
        let snapshot = world
            .app
            .create_snapshot(
                host,
                "before",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("snapshot");
        let group = world.app.create_group("G").expect("group");
        world
            .app
            .set_client_group(id, Some(group.id))
            .expect("assign");
        world.app.set_client_favorite(id, true).expect("favorite");

        world
            .app
            .restore_snapshot(
                host,
                snapshot.id,
                SnapshotRestoreOptions::default(),
                &CancellationToken::new(),
            )
            .expect("restore");

        let view = world.app.organization().unwrap();
        assert!(view.is_favorite_client(id));
        assert_eq!(view.group_of(id), Some(group));
    }

    #[test]
    fn a_promotion_keeps_organization_because_the_id_is_kept() {
        let world = world();
        let id = world.client("repro");
        let png = world.image("icon.png", 16, 16, ImageFormat::Png);
        let group = world.app.create_group("G").expect("group");
        world
            .app
            .set_client_group(id, Some(group.id))
            .expect("assign");
        world.app.set_client_favorite(id, true).expect("favorite");
        world.app.set_client_icon(id, &png).expect("icon");

        let outcome = world
            .app
            .promote_instance(
                id,
                &PromotionRequest::client_only(),
                &OperationReporter::silent("operation-promoting-instance"),
            )
            .expect("promote");
        assert_eq!(outcome.instance.storage, StorageClass::Persistent);

        let view = world.app.organization().unwrap();
        assert!(view.is_favorite_client(id));
        assert_eq!(view.group_of(id), Some(group));
        assert!(view.client_icon(id).is_some());
    }

    #[test]
    fn a_duplicate_inherits_group_and_icon_but_not_favorite() {
        let world = world();
        let original = world.client("repro");
        let png = world.image("icon.png", 16, 16, ImageFormat::Png);
        let group = world.app.create_group("G").expect("group");
        world
            .app
            .set_client_group(original, Some(group.id))
            .expect("assign");
        world
            .app
            .set_client_favorite(original, true)
            .expect("favorite");
        world.app.set_client_icon(original, &png).expect("icon");

        let copy = world
            .app
            .duplicate_client(
                original,
                &DuplicateClient::default(),
                ephervex_model::AbsentModPolicy::Keep,
            )
            .expect("duplicate")
            .record
            .id;

        let view = world.app.organization().unwrap();
        assert_eq!(view.group_of(copy), Some(group));
        assert_eq!(view.client_icon(copy), view.client_icon(original));
        assert!(view.client_icon(copy).is_some());
        assert!(!view.is_favorite_client(copy));
        assert!(view.is_favorite_client(original));
    }

    #[test]
    fn a_valid_icon_is_scaled_and_stored_once_by_content() {
        let world = world();
        let first = world.client("a");
        let second = world.client("b");
        let big = world.image("big.png", 1000, 400, ImageFormat::Png);

        world.app.set_client_icon(first, &big).expect("icon");
        world.app.set_client_icon(second, &big).expect("icon");

        let view = world.reopened().organization().unwrap();
        let stored = view.client_icon(first).expect("stored");
        assert_eq!(view.client_icon(second), Some(stored.clone()));
        assert_eq!(world.icon_files(), 1);
        let decoded = image::open(&stored).expect("a PNG");
        assert_eq!((decoded.width(), decoded.height()), (256, 102));
        let name = stored.file_name().unwrap().to_string_lossy().into_owned();
        assert!(IconId::parse(name.trim_end_matches(".png")).is_some());
    }

    #[test]
    fn a_jpeg_icon_is_accepted_and_stored_as_png() {
        let world = world();
        let id = world.client("a");
        let jpeg = world.image("photo.jpg", 64, 64, ImageFormat::Jpeg);
        world.app.set_client_icon(id, &jpeg).expect("icon");
        let stored = world
            .app
            .organization()
            .unwrap()
            .client_icon(id)
            .expect("stored");
        assert_eq!(
            image::ImageReader::open(&stored)
                .unwrap()
                .with_guessed_format()
                .unwrap()
                .format(),
            Some(ImageFormat::Png)
        );
    }

    #[test]
    fn an_icon_file_goes_once_neither_the_organization_nor_its_previous_copy_names_it() {
        let world = world();
        let id = world.client("a");
        let one = world.image("one.png", 16, 16, ImageFormat::Png);
        let two = {
            let path = world.base.path().join("two.png");
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                16,
                16,
                image::Rgba([1, 2, 3, 255]),
            ))
            .save(&path)
            .expect("write");
            path
        };
        world.app.set_client_icon(id, &one).expect("icon");
        world.app.set_client_icon(id, &two).expect("replace");
        assert_eq!(
            world.icon_files(),
            2,
            "the previous copy still names the first"
        );
        world.app.clear_client_icon(id).expect("clear");
        assert_eq!(world.icon_files(), 1);
        world.app.set_client_favorite(id, true).expect("favorite");
        assert_eq!(world.icon_files(), 0);
        assert!(!world.app.organization().unwrap().client_has_icon(id));
    }

    #[test]
    fn a_shared_icon_survives_until_its_last_host_clears_it() {
        let world = world();
        let first = world.client("a");
        let second = world.client("b");
        let png = world.image("icon.png", 16, 16, ImageFormat::Png);
        world.app.set_client_icon(first, &png).expect("icon");
        world.app.set_client_icon(second, &png).expect("icon");
        world.app.clear_client_icon(first).expect("clear");
        assert_eq!(world.icon_files(), 1);
        world.app.clear_client_icon(second).expect("clear");
        world
            .app
            .set_client_favorite(first, true)
            .expect("favorite");
        assert_eq!(
            world.icon_files(),
            0,
            "gone once its previous copy is replaced too"
        );
    }

    #[test]
    fn a_missing_icon_file_reads_as_no_icon() {
        let world = world();
        let id = world.client("a");
        let png = world.image("icon.png", 16, 16, ImageFormat::Png);
        world.app.set_client_icon(id, &png).expect("icon");
        let stored = world
            .app
            .organization()
            .unwrap()
            .client_icon(id)
            .expect("stored");
        std::fs::remove_file(stored).expect("remove");
        let view = world.app.organization().unwrap();
        assert!(view.client_has_icon(id));
        assert!(view.client_icon(id).is_none());
    }

    #[test]
    fn server_icons_are_stored_and_cleared() {
        let world = world();
        let server = world.server("s");
        let png = world.image("icon.png", 16, 16, ImageFormat::Png);
        world.app.set_server_icon(server, &png).expect("icon");
        assert!(
            world
                .app
                .organization()
                .unwrap()
                .server_icon(server)
                .is_some()
        );
        world.app.clear_server_icon(server).expect("clear");
        assert!(
            world
                .app
                .organization()
                .unwrap()
                .server_icon(server)
                .is_none()
        );
    }

    #[test]
    fn icons_that_cannot_be_used_are_refused_with_a_reason_and_store_nothing() {
        let world = world();
        let id = world.client("a");
        let oversize = world.base.path().join("big.png");
        std::fs::write(
            &oversize,
            vec![0_u8; usize::try_from(MAX_ICON_SOURCE_BYTES).unwrap() + 1],
        )
        .unwrap();
        let text = world.base.path().join("note.png");
        std::fs::write(&text, b"not an image at all").unwrap();
        let gif = world.image("anim.gif", 8, 8, ImageFormat::Gif);
        let wide = world.image("wide.png", 8193, 1, ImageFormat::Png);
        let bomb = world.base.path().join("bomb.png");
        std::fs::write(&bomb, png_header_claiming(8192, 2049)).unwrap();

        let cases = [
            (oversize, IconRefusal::TooLarge),
            (text, IconRefusal::NotAnImage),
            (gif, IconRefusal::UnsupportedFormat),
            (wide, IconRefusal::TooManyPixels),
            (bomb, IconRefusal::TooManyPixels),
            (world.base.path().to_path_buf(), IconRefusal::NotAFile),
            (
                world.base.path().join("missing.png"),
                IconRefusal::Unreadable,
            ),
        ];
        for (path, expected) in cases {
            let error = world.app.set_client_icon(id, &path).expect_err("refused");
            assert_eq!(refusal(error), expected, "{}", path.display());
        }
        assert_eq!(world.icon_files(), 0);
        assert!(!world.app.organization().unwrap().client_has_icon(id));
    }

    #[test]
    fn a_picture_larger_than_artwork_may_be_is_scaled_down_to_an_icon() {
        let world = world();
        let id = world.client("a");
        let photo = world.image("photo.png", 2100, 600, ImageFormat::Png);
        world.app.set_client_icon(id, &photo).expect("accepted");
        let stored = world.app.organization().unwrap().client_icon(id).unwrap();
        let (width, height) = image::image_dimensions(stored).unwrap();
        assert_eq!((width, height), (ICON_MAX_SIDE, 73));
    }

    /// The start of a PNG whose header claims a size, with an empty data chunk.
    fn png_header_claiming(width: u32, height: u32) -> Vec<u8> {
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = !0_u32;
            for byte in bytes {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = if crc & 1 == 1 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        let mut chunk = b"IHDR".to_vec();
        chunk.extend_from_slice(&width.to_be_bytes());
        chunk.extend_from_slice(&height.to_be_bytes());
        chunk.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        for chunk in [chunk, b"IDAT".to_vec()] {
            png.extend_from_slice(&(u32::try_from(chunk.len()).unwrap() - 4).to_be_bytes());
            png.extend_from_slice(&chunk);
            png.extend_from_slice(&crc32(&chunk).to_be_bytes());
        }
        png
    }

    #[test]
    fn new_groups_take_the_least_used_color_and_keep_it_through_a_rename() {
        let world = world();
        let tones: Vec<u8> = ["a", "b", "c"]
            .into_iter()
            .map(|name| world.app.create_group(name).expect("create").tone)
            .collect();
        assert_eq!(tones, [0, 1, 2]);

        let first = world.app.organization().unwrap().group_named("a").unwrap();
        world.app.delete_group(first.id).expect("delete");
        assert_eq!(world.app.create_group("d").expect("create").tone, 0);

        let c = world.app.organization().unwrap().group_named("c").unwrap();
        let renamed = world.app.rename_group(c.id, "aaa").expect("rename");
        assert_eq!(renamed.tone, 2);
        let view = world.reopened().organization().unwrap();
        assert_eq!(view.group_named("aaa").unwrap().tone, 2);
    }

    #[test]
    fn a_refused_icon_leaves_the_previous_icon_in_place() {
        let world = world();
        let id = world.client("a");
        let png = world.image("icon.png", 16, 16, ImageFormat::Png);
        world.app.set_client_icon(id, &png).expect("icon");
        let text = world.base.path().join("note.png");
        std::fs::write(&text, b"nope").unwrap();
        assert!(world.app.set_client_icon(id, &text).is_err());
        assert!(world.app.organization().unwrap().client_icon(id).is_some());
    }

    #[test]
    fn the_chosen_sort_survives_a_restart_and_an_unknown_one_falls_back() {
        let mut world = world();
        assert_eq!(world.app.instance_sort(), InstanceSort::Favorite);
        world
            .app
            .set_instance_sort(InstanceSort::Loader)
            .expect("set");
        assert_eq!(world.reopened().instance_sort(), InstanceSort::Loader);

        world
            .app
            .mutate_config(|config| "popularity".clone_into(&mut config.display.instance_sort))
            .expect("write a value this build does not know");
        assert_eq!(world.reopened().instance_sort(), InstanceSort::Favorite);
    }

    #[test]
    fn instance_facts_carry_favorites_groups_and_linking() {
        let world = world();
        let paired = world.client("paired");
        let lone = world.client("lone");
        let staging = world.base.path().join("staged-link");
        std::fs::create_dir_all(staging.join("server")).unwrap();
        let record = world
            .app
            .registry_for_services()
            .prepare_server(NewServer::linked("linked", paired))
            .expect("prepare");
        world
            .app
            .registry_for_services()
            .publish_staged_server(&record, &staging)
            .expect("publish");
        world.app.set_client_favorite(lone, true).expect("favorite");

        let facts = world.app.instance_facts().expect("facts");
        let find = |id: InstanceId| facts.iter().find(|facts| facts.id == id).expect("listed");
        assert_eq!(find(paired).linking, crate::LinkingState::Paired);
        assert_eq!(find(lone).linking, crate::LinkingState::None);
        assert!(find(lone).favorite);
        assert!(!find(paired).running);
    }

    #[test]
    fn clearing_a_pair_moves_the_client_to_linked_servers_not_paired() {
        let mut world = world();
        let client = world.client("paired");
        let staging = world.base.path().join("staged-link");
        std::fs::create_dir_all(staging.join("server")).unwrap();
        let record = world
            .app
            .registry_for_services()
            .prepare_server(NewServer::linked("linked", client))
            .expect("prepare");
        world
            .app
            .registry_for_services()
            .publish_staged_server(&record, &staging)
            .expect("publish");
        assert_eq!(
            world.app.linking_states().unwrap()[&client],
            crate::LinkingState::Paired
        );

        world.app.clear_linked_pair(client).expect("unpair");

        assert_eq!(
            world.app.linking_states().unwrap()[&client],
            crate::LinkingState::LinkedServers
        );
    }

    #[test]
    fn a_stale_pair_entry_counts_as_not_paired() {
        let mut world = world();
        let client = world.client("c");
        world
            .app
            .mutate_config(|config| {
                config.linked_pairs.insert(
                    client.to_string(),
                    ephervex_storage::LinkedPairConfig {
                        server: ServerId::new().to_string(),
                        ..ephervex_storage::LinkedPairConfig::default()
                    },
                );
            })
            .expect("write a stale entry");
        assert_eq!(
            world.app.linking_states().unwrap()[&client],
            crate::LinkingState::None
        );
    }

    #[test]
    fn a_diagnostic_bundle_never_offers_organization_files() {
        let world = world();
        let id = world.client("repro");
        let png = world.image("icon.png", 16, 16, ImageFormat::Png);
        world.app.set_client_favorite(id, true).expect("favorite");
        world.app.set_client_icon(id, &png).expect("icon");

        for scope in [DiagnosticScope::Client(id), DiagnosticScope::Launcher] {
            let contents = world.app.plan_diagnostic_bundle(scope).expect("plan");
            for part in &contents.parts {
                assert!(!part.name.contains("organization"), "{}", part.name);
                assert!(
                    !part.path.starts_with(world.app.paths().icons_dir()),
                    "{}",
                    part.name
                );
                assert_ne!(part.path, world.app.paths().organization_file());
            }
        }
    }

    #[test]
    fn a_damaged_organization_file_is_set_aside_and_reported() {
        let world = world();
        let id = world.client("repro");
        std::fs::create_dir_all(world.app.paths().data_dir()).unwrap();
        std::fs::write(world.app.paths().organization_file(), "not json").unwrap();

        let view = world
            .app
            .organization()
            .expect("a damaged file does not stop reading");
        assert!(
            take_organization_notices()
                .iter()
                .any(|notice| notice.set_aside.starts_with(world.app.paths().data_dir()))
        );
        assert!(!view.is_favorite_client(id));
        world
            .app
            .set_client_favorite(id, true)
            .expect("writes start from empty");
        assert!(world.app.organization().unwrap().is_favorite_client(id));
    }
}
