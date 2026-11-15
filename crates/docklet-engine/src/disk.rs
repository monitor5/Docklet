//! What `docker system df -v` knows: the disk used by images, containers,
//! volumes and the build cache, and how much of it could be reclaimed.
//!
//! Since API 1.52 the engine returns each kind as a section with totals and,
//! when asked for verbose output, its items as untyped JSON. This module
//! reads the fields it needs from them and leaves the rest.

use bollard::models::SystemDataUsageResponse;
use serde_json::Value;

/// One image, as a row of `docker system df -v`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDisk {
    pub id: String,
    /// `repo:tag` names; empty for a dangling image.
    pub tags: Vec<String>,
    /// All layers of the image, shared ones included.
    pub size: u64,
    /// The part of `size` in layers other images use too. `None` when the
    /// engine did not work it out.
    pub shared_size: Option<u64>,
    /// Containers created from the image, running or not. `None` when the
    /// engine did not count them.
    pub containers: Option<u64>,
    /// Unix seconds.
    pub created: i64,
}

impl ImageDisk {
    /// What removing only this image would free: `size - shared_size`.
    pub fn unique_size(&self) -> Option<u64> {
        self.shared_size
            .map(|shared| self.size.saturating_sub(shared))
    }
}

/// One container's writable layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerDisk {
    pub id: String,
    /// The first name, without Docker's leading `/`.
    pub name: String,
    pub image: String,
    /// `running`, `exited`, ...
    pub state: String,
    /// Files the container created or changed.
    pub size_rw: u64,
    /// The whole root file system, image layers included.
    pub size_root_fs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeDisk {
    pub name: String,
    pub driver: String,
    /// `None` for a driver that cannot tell (only `local` can).
    pub size: Option<u64>,
    /// Containers that mount the volume, running or not. `None` when unknown.
    pub ref_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCacheDisk {
    pub id: String,
    /// `regular`, `source.local`, `exec.cachemount`, ...
    pub kind: String,
    pub description: String,
    pub size: u64,
    pub in_use: bool,
    pub shared: bool,
}

/// Totals for one kind, as the summary of `docker system df` shows them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskTotals {
    pub count: u64,
    /// Images with containers, running containers, mounted volumes, cache
    /// records in use.
    pub active: u64,
    pub size: u64,
    /// What the matching prune would free.
    pub reclaimable: u64,
}

/// The engine's disk usage, kind by kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskUsage {
    pub images: Vec<ImageDisk>,
    pub containers: Vec<ContainerDisk>,
    pub volumes: Vec<VolumeDisk>,
    pub build_cache: Vec<BuildCacheDisk>,
    pub image_totals: DiskTotals,
    pub container_totals: DiskTotals,
    pub volume_totals: DiskTotals,
    pub build_cache_totals: DiskTotals,
}

impl DiskUsage {
    /// Reads a `GET /system/df?verbose=true` answer. The engine's own totals
    /// are used where it sent them; otherwise they are added up from the
    /// items the way the Docker CLI did before the engine reported them.
    pub fn from_response(response: &SystemDataUsageResponse) -> Self {
        let images: Vec<_> = section_items(response.image_usage.as_ref().map(|u| &u.items))
            .filter_map(image)
            .collect();
        let containers: Vec<_> = section_items(response.container_usage.as_ref().map(|u| &u.items))
            .filter_map(container)
            .collect();
        let volumes: Vec<_> = section_items(response.volume_usage.as_ref().map(|u| &u.items))
            .filter_map(volume)
            .collect();
        let build_cache: Vec<_> =
            section_items(response.build_cache_usage.as_ref().map(|u| &u.items))
                .filter_map(build_cache_record)
                .collect();

        let image_totals = response.image_usage.as_ref().map_or_else(
            || image_totals(&images),
            |u| {
                reported(
                    image_totals(&images),
                    [u.total_count, u.active_count, u.total_size, u.reclaimable],
                )
            },
        );
        let container_totals = response.container_usage.as_ref().map_or_else(
            || container_totals(&containers),
            |u| {
                reported(
                    container_totals(&containers),
                    [u.total_count, u.active_count, u.total_size, u.reclaimable],
                )
            },
        );
        let volume_totals = response.volume_usage.as_ref().map_or_else(
            || volume_totals(&volumes),
            |u| {
                reported(
                    volume_totals(&volumes),
                    [u.total_count, u.active_count, u.total_size, u.reclaimable],
                )
            },
        );
        let build_cache_totals = response.build_cache_usage.as_ref().map_or_else(
            || build_cache_totals(&build_cache),
            |u| {
                reported(
                    build_cache_totals(&build_cache),
                    [u.total_count, u.active_count, u.total_size, u.reclaimable],
                )
            },
        );

        Self {
            images,
            containers,
            volumes,
            build_cache,
            image_totals,
            container_totals,
            volume_totals,
            build_cache_totals,
        }
    }

    /// Disk used by all four kinds.
    pub fn total_size(&self) -> u64 {
        self.all_totals().map(|totals| totals.size).sum()
    }

    /// What pruning all four kinds would free.
    pub fn total_reclaimable(&self) -> u64 {
        self.all_totals().map(|totals| totals.reclaimable).sum()
    }

    fn all_totals(&self) -> impl Iterator<Item = DiskTotals> {
        [
            self.image_totals,
            self.container_totals,
            self.volume_totals,
            self.build_cache_totals,
        ]
        .into_iter()
    }
}

/// The items of one section, or none.
fn section_items(items: Option<&Option<Vec<Value>>>) -> impl Iterator<Item = &Value> {
    items.and_then(Option::as_ref).into_iter().flatten()
}

/// `computed`, with each figure the engine reported in its place.
/// `reported` is count, active, size and reclaimable, in that order.
fn reported(computed: DiskTotals, reported: [Option<i64>; 4]) -> DiskTotals {
    let [count, active, size, reclaimable] = reported.map(|value| value.and_then(non_negative));
    DiskTotals {
        count: count.unwrap_or(computed.count),
        active: active.unwrap_or(computed.active),
        size: size.unwrap_or(computed.size),
        reclaimable: reclaimable.unwrap_or(computed.reclaimable),
    }
}

/// Docker reports "not known" as -1.
fn non_negative(value: i64) -> Option<u64> {
    u64::try_from(value).ok()
}

fn string(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number(item: &Value, key: &str) -> Option<u64> {
    item.get(key).and_then(Value::as_i64).and_then(non_negative)
}

fn strings(item: &Value, key: &str) -> Vec<String> {
    let values = item
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    values
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn image(item: &Value) -> Option<ImageDisk> {
    let id = string(item, "Id");
    if id.is_empty() {
        return None;
    }
    // Untagged images used to be listed with a `<none>:<none>` tag.
    let tags = strings(item, "RepoTags")
        .into_iter()
        .filter(|tag| tag != "<none>:<none>")
        .collect();
    Some(ImageDisk {
        id,
        tags,
        size: number(item, "Size").unwrap_or(0),
        shared_size: number(item, "SharedSize"),
        containers: number(item, "Containers"),
        created: item.get("Created").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn container(item: &Value) -> Option<ContainerDisk> {
    let id = string(item, "Id");
    if id.is_empty() {
        return None;
    }
    let name = strings(item, "Names")
        .first()
        .map(|name| name.trim_start_matches('/').to_string())
        .unwrap_or_default();
    Some(ContainerDisk {
        id,
        name,
        image: string(item, "Image"),
        state: string(item, "State"),
        size_rw: number(item, "SizeRw").unwrap_or(0),
        size_root_fs: number(item, "SizeRootFs").unwrap_or(0),
    })
}

fn volume(item: &Value) -> Option<VolumeDisk> {
    let name = string(item, "Name");
    if name.is_empty() {
        return None;
    }
    let usage = item.get("UsageData");
    Some(VolumeDisk {
        name,
        driver: string(item, "Driver"),
        size: usage.and_then(|usage| number(usage, "Size")),
        ref_count: usage.and_then(|usage| number(usage, "RefCount")),
    })
}

fn build_cache_record(item: &Value) -> Option<BuildCacheDisk> {
    let id = string(item, "ID");
    if id.is_empty() {
        return None;
    }
    let flag = |key| item.get(key).and_then(Value::as_bool).unwrap_or(false);
    Some(BuildCacheDisk {
        id,
        kind: string(item, "Type"),
        description: string(item, "Description"),
        size: number(item, "Size").unwrap_or(0),
        in_use: flag("InUse"),
        shared: flag("Shared"),
    })
}

/// Without the engine's figure the size counts shared layers once per image,
/// so it is an upper bound; reclaimable is what images without containers
/// hold beyond their shared layers subtracted from it, as the CLI did.
fn image_totals(images: &[ImageDisk]) -> DiskTotals {
    let size: u64 = images.iter().map(|image| image.size).sum();
    let used: u64 = images
        .iter()
        .filter(|image| image.containers.is_some_and(|count| count > 0))
        .filter_map(ImageDisk::unique_size)
        .sum();
    DiskTotals {
        count: images.len() as u64,
        active: images
            .iter()
            .filter(|image| image.containers.is_some_and(|count| count > 0))
            .count() as u64,
        size,
        reclaimable: size.saturating_sub(used),
    }
}

/// A container that runs, is paused or restarts holds on to its layer.
fn container_is_active(container: &ContainerDisk) -> bool {
    matches!(
        container.state.as_str(),
        "running" | "paused" | "restarting"
    )
}

fn container_totals(containers: &[ContainerDisk]) -> DiskTotals {
    let (active, idle): (Vec<_>, Vec<_>) = containers.iter().partition(|c| container_is_active(c));
    DiskTotals {
        count: containers.len() as u64,
        active: active.len() as u64,
        size: containers.iter().map(|c| c.size_rw).sum(),
        reclaimable: idle.iter().map(|c| c.size_rw).sum(),
    }
}

fn volume_totals(volumes: &[VolumeDisk]) -> DiskTotals {
    let in_use = |volume: &&VolumeDisk| volume.ref_count.is_some_and(|count| count > 0);
    DiskTotals {
        count: volumes.len() as u64,
        active: volumes.iter().filter(in_use).count() as u64,
        size: volumes.iter().filter_map(|v| v.size).sum(),
        reclaimable: volumes
            .iter()
            .filter(|volume| volume.ref_count == Some(0))
            .filter_map(|volume| volume.size)
            .sum(),
    }
}

/// Shared records belong to more than this engine's cache and are left out,
/// as the CLI does.
fn build_cache_totals(records: &[BuildCacheDisk]) -> DiskTotals {
    let own = || records.iter().filter(|record| !record.shared);
    DiskTotals {
        count: records.len() as u64,
        active: records.iter().filter(|record| record.in_use).count() as u64,
        size: own().map(|record| record.size).sum(),
        reclaimable: own()
            .filter(|record| !record.in_use)
            .map(|record| record.size)
            .sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `GET /system/df?verbose=true` from an API 1.52 engine with two
    /// images sharing a base layer, a running and an exited container, two
    /// volumes and a little build cache.
    const VERBOSE: &str = r#"{
        "ImageUsage": {
            "ActiveCount": 1,
            "TotalCount": 3,
            "Reclaimable": 90000000,
            "TotalSize": 250000000,
            "Items": [
                {
                    "Id": "sha256:aaa", "ParentId": "", "RepoTags": ["nginx:latest", "web:1"],
                    "RepoDigests": [], "Created": 1760000000, "Size": 190000000,
                    "SharedSize": 80000000, "Labels": {}, "Containers": 2
                },
                {
                    "Id": "sha256:bbb", "ParentId": "", "RepoTags": ["debian:13"],
                    "RepoDigests": [], "Created": 1750000000, "Size": 120000000,
                    "SharedSize": 80000000, "Labels": null, "Containers": 0
                },
                {
                    "Id": "sha256:ccc", "ParentId": "", "RepoTags": null,
                    "RepoDigests": [], "Created": 1740000000, "Size": 20000000,
                    "SharedSize": -1, "Labels": null, "Containers": -1
                }
            ]
        },
        "ContainerUsage": {
            "ActiveCount": 1,
            "TotalCount": 2,
            "Reclaimable": 4096,
            "TotalSize": 6144,
            "Items": [
                {
                    "Id": "c1", "Names": ["/web"], "Image": "nginx:latest",
                    "ImageID": "sha256:aaa", "State": "running", "Status": "Up 2 hours",
                    "SizeRw": 2048, "SizeRootFs": 190002048
                },
                {
                    "Id": "c2", "Names": ["/old"], "Image": "web:1",
                    "ImageID": "sha256:aaa", "State": "exited", "Status": "Exited (0) 3 days ago",
                    "SizeRw": 4096, "SizeRootFs": 190004096
                }
            ]
        },
        "VolumeUsage": {
            "ActiveCount": 1,
            "TotalCount": 3,
            "Reclaimable": 1000,
            "TotalSize": 6000,
            "Items": [
                {
                    "Name": "data", "Driver": "local", "Mountpoint": "/var/lib/docker/volumes/data/_data",
                    "Labels": null, "Scope": "local", "Options": null,
                    "UsageData": { "Size": 5000, "RefCount": 1 }
                },
                {
                    "Name": "cache", "Driver": "local", "Mountpoint": "/var/lib/docker/volumes/cache/_data",
                    "Labels": null, "Scope": "local", "Options": null,
                    "UsageData": { "Size": 1000, "RefCount": 0 }
                },
                {
                    "Name": "remote", "Driver": "nfs", "Mountpoint": "",
                    "Labels": null, "Scope": "local", "Options": null,
                    "UsageData": { "Size": -1, "RefCount": -1 }
                }
            ]
        },
        "BuildCacheUsage": {
            "ActiveCount": 1,
            "TotalCount": 3,
            "Reclaimable": 300,
            "TotalSize": 800,
            "Items": [
                { "ID": "r1", "Type": "regular", "Description": "RUN apt-get update",
                  "InUse": false, "Shared": false, "Size": 300, "UsageCount": 2 },
                { "ID": "r2", "Type": "exec.cachemount", "Description": "cache mount",
                  "InUse": true, "Shared": false, "Size": 500, "UsageCount": 9 },
                { "ID": "r3", "Type": "regular", "Description": "FROM debian",
                  "InUse": false, "Shared": true, "Size": 7000, "UsageCount": 1 }
            ]
        }
    }"#;

    fn parse(json: &str) -> DiskUsage {
        DiskUsage::from_response(&serde_json::from_str(json).expect("a valid df fixture"))
    }

    /// The fixture with the engine's totals taken out of every section.
    fn without_totals() -> DiskUsage {
        let mut value: Value = serde_json::from_str(VERBOSE).unwrap();
        for section in value.as_object_mut().unwrap().values_mut() {
            let section = section.as_object_mut().unwrap();
            section.retain(|key, _| key == "Items");
        }
        parse(&value.to_string())
    }

    #[test]
    fn items_are_read_from_each_section() {
        let usage = parse(VERBOSE);
        assert_eq!(usage.images.len(), 3);
        let nginx = &usage.images[0];
        assert_eq!(nginx.tags, ["nginx:latest", "web:1"]);
        assert_eq!(nginx.size, 190_000_000);
        assert_eq!(nginx.unique_size(), Some(110_000_000));
        assert_eq!(nginx.containers, Some(2));
        let dangling = &usage.images[2];
        assert!(dangling.tags.is_empty());
        assert_eq!(dangling.shared_size, None);
        assert_eq!(dangling.unique_size(), None);
        assert_eq!(dangling.containers, None);

        assert_eq!(usage.containers[0].name, "web");
        assert_eq!(usage.containers[1].size_rw, 4096);
        assert_eq!(usage.containers[1].size_root_fs, 190_004_096);

        assert_eq!(usage.volumes[0].size, Some(5000));
        assert_eq!(usage.volumes[1].ref_count, Some(0));
        assert_eq!(usage.volumes[2].size, None);
        assert_eq!(usage.volumes[2].driver, "nfs");

        assert_eq!(usage.build_cache.len(), 3);
        assert!(usage.build_cache[1].in_use);
        assert_eq!(usage.build_cache[1].kind, "exec.cachemount");
    }

    #[test]
    fn the_engines_totals_win() {
        let usage = parse(VERBOSE);
        assert_eq!(
            usage.image_totals,
            DiskTotals {
                count: 3,
                active: 1,
                size: 250_000_000,
                reclaimable: 90_000_000,
            }
        );
        assert_eq!(usage.volume_totals.count, 3);
        assert_eq!(usage.build_cache_totals.size, 800);
        assert_eq!(usage.total_size(), 250_000_000 + 6144 + 6000 + 800);
        assert_eq!(usage.total_reclaimable(), 90_000_000 + 4096 + 1000 + 300);
    }

    #[test]
    fn totals_are_added_up_when_the_engine_leaves_them_out() {
        let usage = without_totals();
        // Shared layers count once per image: an upper bound.
        assert_eq!(
            usage.image_totals,
            DiskTotals {
                count: 3,
                active: 1,
                size: 330_000_000,
                // All but nginx's own layers.
                reclaimable: 220_000_000,
            }
        );
        assert_eq!(
            usage.container_totals,
            DiskTotals {
                count: 2,
                active: 1,
                size: 6144,
                reclaimable: 4096,
            }
        );
        // The nfs volume has no size and counts as neither.
        assert_eq!(
            usage.volume_totals,
            DiskTotals {
                count: 3,
                active: 1,
                size: 6000,
                reclaimable: 1000,
            }
        );
        // The shared record is not this engine's to free.
        assert_eq!(
            usage.build_cache_totals,
            DiskTotals {
                count: 3,
                active: 1,
                size: 800,
                reclaimable: 300,
            }
        );
    }

    #[test]
    fn a_negative_total_is_unknown_and_falls_back() {
        let json = r#"{
            "VolumeUsage": {
                "TotalSize": -1,
                "Reclaimable": 10,
                "Items": [{ "Name": "v", "Driver": "local", "Mountpoint": "", "Labels": null,
                            "Options": null, "UsageData": { "Size": 64, "RefCount": 0 } }]
            }
        }"#;
        let usage = parse(json);
        assert_eq!(usage.volume_totals.size, 64);
        assert_eq!(usage.volume_totals.reclaimable, 10);
        assert_eq!(usage.volume_totals.count, 1);
    }

    #[test]
    fn an_empty_engine_uses_nothing() {
        let usage = parse("{}");
        assert_eq!(usage, DiskUsage::default());
        assert_eq!(usage.total_size(), 0);
        assert_eq!(usage.total_reclaimable(), 0);
    }

    #[test]
    fn items_without_an_id_are_skipped() {
        let json = r#"{
            "ContainerUsage": { "Items": [{ "Names": ["/ghost"], "SizeRw": 5 }, "junk"] }
        }"#;
        assert!(parse(json).containers.is_empty());
    }
}
