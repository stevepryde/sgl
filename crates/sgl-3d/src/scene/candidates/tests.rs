//! Placing draw candidates near what the device binds, without a device.
use super::*;
use crate::shading::culling::NO_CHAIN;
use wasm_bindgen_test::wasm_bindgen_test;

/// The most draw instances each view's cluster list binds in these tests.
const REGIONS: u32 = 1000;

/// Candidates for a device whose storage bindings hold `REGIONS` draw
/// instances.
fn candidates() -> Candidates {
    Candidates::new(&wgpu::Limits {
        max_storage_buffer_binding_size: u64::from(REGIONS)
            * std::mem::size_of::<DrawInstance>() as u64,
        max_buffer_size: u64::MAX,
        ..wgpu::Limits::default()
    })
}

/// Candidates for a device that binds a gibibyte.
fn roomy() -> Candidates {
    Candidates::new(&wgpu::Limits {
        max_storage_buffer_binding_size: 1 << 30,
        max_buffer_size: u64::MAX,
        ..wgpu::Limits::default()
    })
}

/// A mesh drawn with material `material` of at most `need` sections.
fn mesh(material: usize, need: u32) -> CandidateMesh {
    CandidateMesh {
        material: MaterialId::issue(material, 1),
        look: SetLook::default(),
        blended: false,
        need,
        bounds: [Vec3::ZERO; 2],
        word: 0,
        chain: NO_CHAIN,
        positions: (0, 0),
    }
}

fn place(candidates: &mut Candidates, index: usize, meshes: &[CandidateMesh]) -> bool {
    let model = ModelId::issue(0, 1);
    candidates
        .place(index, (model, meshes), false, None)
        .is_ok()
}

/// Each live set's region, in index order.
fn regions(candidates: &Candidates) -> Vec<Range<u32>> {
    candidates
        .sets
        .iter()
        .map(|(_, _, region)| region)
        .collect()
}

// Plausible defect: a refused placement undoing its sets' accounting by
// removing what it added, which leaves a set whose region adding re-placed
// at the end still there, so the regions pass what the device binds and the
// next frame's cull bind group fails validation. The oracle is the limit
// and the regions before the refusal: a refused placement leaves both.
#[wasm_bindgen_test(unsupported = test)]
fn a_refused_placement_leaves_every_region_where_it_was() {
    let mut candidates = candidates();
    // Material 0's set holds 150 at the start, material 1's 750 after it.
    assert!(place(&mut candidates, 0, &[mesh(0, 100)]));
    assert!(place(&mut candidates, 1, &[mesh(1, 500)]));
    let before = regions(&candidates);
    // Material 0's set must grow past its region, to the end, past the
    // limit.
    assert!(!place(&mut candidates, 2, &[mesh(0, 70)]));
    assert_eq!(regions(&candidates), before);
    assert!(candidates.sets.fit(REGIONS), "the regions fit the device");
    assert_eq!(candidates.end(), 2, "no slot taken");
}

// Plausible defect: a placement that counts an instance's new candidates
// before it frees the ones they replace, so re-placing an instance near the
// limit is refused although what it holds afterwards fits. The oracle is
// the limit: an instance placed again as it was holds what it held.
#[wasm_bindgen_test(unsupported = test)]
fn an_instance_placed_again_near_the_limit_fits() {
    let mut candidates = candidates();
    assert!(place(&mut candidates, 0, &[mesh(0, 600)]));
    assert!(place(&mut candidates, 0, &[mesh(0, 600)]));
    assert!(candidates.fit([(0, Candidates::keys(&[mesh(0, 600)], false, false))]));
}

// Plausible defects: an edit's dry run that bounds what re-placing many
// instances takes too low (counting only the sections it adds, ignoring a
// set's region re-placed at half again its need, or a second model's
// instances placed after the first's), so the edit commits and its
// placement fails, or too high, refusing edits that fit. The oracle is the
// placement itself: from the same state, the dry run of a plan says it fits
// exactly when placing it instance by instance succeeds throughout and
// leaves every region and slot within the device's limits.
#[wasm_bindgen_test(unsupported = test)]
fn a_dry_run_fits_exactly_when_the_placements_do() {
    // A plan whose first placement passes the limit and whose second
    // brings the regions back within it: placing it fails at the first.
    let mut midway = candidates();
    assert!(place(&mut midway, 0, &[mesh(0, 100)]));
    assert!(place(&mut midway, 1, &[mesh(1, 500)]));
    let plan = [
        (2, Candidates::keys(&[mesh(0, 60)], false, false)),
        (0, Vec::new()),
    ];
    assert!(!midway.fit(plan), "the plan passes the limit midway");
    assert!(!place(&mut midway, 2, &[mesh(0, 60)]));
    let mut state = 0x2545_f491u32;
    let mut random = |bound: u32| {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) % bound
    };
    let mut agreed = [0; 2];
    for _ in 0..300 {
        // A history of placements and a plan of re-placements, each
        // instance one to three meshes of four materials.
        let history: Vec<(usize, Vec<CandidateMesh>)> = (0..random(12))
            .map(|_| {
                let meshes = (0..1 + random(3))
                    .map(|_| mesh(random(4) as usize, 1 + random(120)))
                    .collect();
                (random(8) as usize, meshes)
            })
            .collect();
        // Distinct instances, as an edit's re-placement lists them.
        let first = random(8) as usize;
        let plan: Vec<(usize, Vec<CandidateMesh>)> = (0..1 + random(4) as usize)
            .map(|step| {
                let meshes = (0..random(4))
                    .map(|_| mesh(random(4) as usize, 1 + random(200)))
                    .collect();
                ((first + step) % 8, meshes)
            })
            .collect();
        let replay = || {
            let mut candidates = candidates();
            for (index, meshes) in &history {
                place(&mut candidates, *index, meshes);
            }
            candidates
        };
        let dry = replay();
        let keys = plan
            .iter()
            .map(|(index, meshes)| (*index, Candidates::keys(meshes, false, false)));
        let fits = dry.fit(keys);
        let mut placed = replay();
        let mut succeeded = true;
        for (index, meshes) in &plan {
            succeeded = place(&mut placed, *index, meshes);
            assert!(
                placed.sets.fit(REGIONS),
                "a placement leaves the regions bound"
            );
            if !succeeded {
                break;
            }
        }
        assert_eq!(fits, succeeded, "the dry run of {plan:?} after {history:?}");
        agreed[usize::from(fits)] += 1;
    }
    assert!(
        agreed.iter().all(|&count| count > 20),
        "plans both fit and not: {agreed:?}"
    );
}

// Plausible defect: the sets' change list taking an entry for every edit of
// a set rather than one for each set changed, so the copy each placement
// takes of the sets grows with the placements since the last upload, and a
// game adding many instances before a frame pays their square. The oracle
// is the sets: however many candidates join them, the records changed are
// at most the sets there are.
#[wasm_bindgen_test(unsupported = test)]
fn many_placements_change_each_set_once() {
    let mut candidates = roomy();
    for index in 0..10_000 {
        assert!(place(&mut candidates, index, &[mesh(index % 2, 1)]));
    }
    assert_eq!(candidates.sets.end(), 2);
    assert!(
        candidates.sets.changed() <= 2,
        "{} set records marked changed",
        candidates.sets.changed()
    );
}
