use super::testgguf::*;
use super::*;
use crate::km::sample;

const HIDDEN: u64 = 8;
const FFN: u64 = 4;
const SLOTS: u64 = 4;
const DIMS: (u32, u32, u32) = (HIDDEN as u32, FFN as u32, SLOTS as u32);

/// A 2-layer core, 4 slots: slot `s` of every expert tensor is filled with `s + 1`.
fn base(dir: &Path) -> PathBuf {
    let mut t = vec![T {
        name: "token_embd.weight".into(),
        dims: vec![HIDDEN, 3],
        ty: F32,
        data: vec![7; (HIDDEN * 3 * 4) as usize],
    }];
    for layer in 0..2 {
        t.extend(exps(layer, SLOTS, HIDDEN, FFN, F16, |_, s| s as u8 + 1));
        t.push(router(layer, SLOTS, HIDDEN, |s| 0x40 + s as u8));
    }
    let p = dir.join("base.gguf");
    write(&p, &t);
    p
}

fn bytes_of(t: &[T]) -> Vec<u8> {
    let d = crate::testing::temp_home();
    write(&d.join("f.gguf"), t);
    let b = std::fs::read(d.join("f.gguf")).unwrap();
    let _ = std::fs::remove_dir_all(&d);
    b
}

/// A fragment for `layer` holding `n` slots, every byte `fill`; with the router rows when `with_router`.
fn fragment(layer: u32, n: u64, fill: u8, with_router: bool) -> Vec<u8> {
    let mut t = exps(layer, n, HIDDEN, FFN, F16, |_, _| fill);
    if with_router {
        t.push(router(layer, n, HIDDEN, |_| fill ^ 0xFF));
    }
    bytes_of(&t)
}

fn km(root: &Path, id: &str, parts: &[(u32, [u32; 2], Vec<u8>)]) -> ExpertsKm {
    ExpertsKm::open(&sample::experts_with(root, id, "1.0.0", DIMS, parts)).unwrap()
}

fn slot_bytes(file: &[u8], h: &Header, name: &str, slot: u64) -> Vec<u8> {
    let t = &h.tensors[name];
    let stride = t.bytes / t.dims.last().unwrap();
    let at = (t.offset + stride * slot) as usize;
    file[at..at + stride as usize].to_vec()
}

#[test]
fn a_kms_slots_replace_exactly_those_slots_and_nothing_else_changes() {
    let home = crate::testing::temp_home();
    let b = base(&home);
    let k = km(
        &home.join("kms"),
        "ex.a",
        &[
            (1, [1, 2], fragment(1, 2, 0xA5, true)),
            (0, [3, 3], fragment(0, 1, 0xB6, false)),
        ],
    );
    let out = assemble(&home, "dusty", &b, &[k]).unwrap();
    assert!(!out.reused);
    assert_eq!(out.slots, 3);
    assert!(out.path.starts_with(assembled_dir(&home)));
    let (base_bytes, new_bytes) = (std::fs::read(&b).unwrap(), std::fs::read(&out.path).unwrap());
    assert_eq!(base_bytes.len(), new_bytes.len());
    let h = read_header(&b).unwrap();
    assert_eq!(
        h.tensors,
        read_header(&out.path).unwrap().tensors,
        "the header is untouched"
    );
    let mut changed = 0;
    for (name, t) in &h.tensors {
        let slots = if name.starts_with("blk.") {
            *t.dims.last().unwrap()
        } else {
            1
        };
        for s in 0..slots {
            let (old, new) = (
                slot_bytes(&base_bytes, &h, name, s),
                slot_bytes(&new_bytes, &h, name, s),
            );
            let layer1 = name.starts_with("blk.1.");
            let replaced =
                (layer1 && (s == 1 || s == 2)) || (name.starts_with("blk.0.") && s == 3 && name.contains("_exps"));
            if replaced {
                changed += 1;
                let want = match (layer1, name.contains("gate_inp")) {
                    (true, true) => 0xA5 ^ 0xFF,
                    (true, false) => 0xA5,
                    _ => 0xB6,
                };
                assert!(new.iter().all(|b| *b == want), "{name} slot {s}");
            } else {
                assert_eq!(old, new, "{name} slot {s} must be untouched");
            }
        }
    }
    // layer 1: (3 expert tensors + the router) x 2 slots; layer 0: 3 expert tensors x 1 slot
    assert_eq!(changed, 8 + 3);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_mismatch_is_refused_with_the_tensor_named_and_leaves_no_file() {
    let home = crate::testing::temp_home();
    let b = base(&home);
    let kms = home.join("kms");
    let frag = |layer, n, ty, hidden| bytes_of(&exps(layer, n, hidden, FFN, ty, |_, _| 1));
    let refuse = |id: &str, parts: &[(u32, [u32; 2], Vec<u8>)], needle: &str| {
        let e = assemble(&home, "dusty", &b, &[km(&kms, id, parts)]).unwrap_err();
        assert!(e.message.contains(needle), "{id}: {}", e.message);
    };
    refuse("ex.quant", &[(0, [0, 0], frag(0, 1, F32, HIDDEN))], "quant type");
    refuse("ex.shape", &[(0, [0, 0], frag(0, 1, F16, HIDDEN * 2))], "shape");
    refuse("ex.count", &[(0, [0, 1], frag(0, 1, F16, HIDDEN))], "holds 1 slots but");
    // the manifest allows slot 4 (it declares 8 per layer), the base has only 4
    let wide = sample::experts_with(
        &kms,
        "ex.range",
        "1.0.0",
        (8, 4, 8),
        &[(0, [4, 4], frag(0, 1, F16, HIDDEN))],
    );
    let e = assemble(&home, "dusty", &b, &[ExpertsKm::open(&wide).unwrap()]).unwrap_err();
    assert!(e.message.contains("slot 4 does not exist"), "{}", e.message);
    refuse(
        "ex.layer",
        &[(0, [0, 0], frag(1, 1, F16, HIDDEN))],
        "not an expert tensor of layer 0",
    );
    refuse(
        "ex.nolayer",
        &[(7, [0, 0], frag(7, 1, F16, HIDDEN))],
        "has no tensor blk.7",
    );
    let mut t = exps(0, 1, HIDDEN, FFN, F16, |_, _| 1);
    t.remove(1);
    refuse("ex.noup", &[(0, [0, 0], bytes_of(&t))], "lacks blk.0.ffn_up_exps");
    refuse("ex.junk", &[(0, [0, 0], vec![1, 2, 3, 4, 5, 6, 7, 8])], "not a");
    // two KMs on one slot
    let a = km(&kms, "ex.one", &[(0, [0, 1], frag(0, 2, F16, HIDDEN))]);
    let c = km(&kms, "ex.two", &[(0, [1, 2], frag(0, 2, F16, HIDDEN))]);
    let e = assemble(&home, "dusty", &b, &[a, c]).unwrap_err();
    assert!(e.message.contains("both fill layer 0 slot 1"), "{}", e.message);
    let made = std::fs::read_dir(assembled_dir(&home)).map(|d| d.count()).unwrap_or(0);
    assert_eq!(made, 0, "a refusal leaves no assembled or partial file");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_same_inputs_reuse_the_file_and_other_inputs_make_another() {
    let home = crate::testing::temp_home();
    let b = base(&home);
    let kms = home.join("kms");
    let one = km(&kms, "ex.one", &[(0, [0, 0], fragment(0, 1, 0x11, false))]);
    let two = km(&kms, "ex.two", &[(1, [2, 2], fragment(1, 1, 0x22, false))]);
    let first = assemble(&home, "dusty", &b, &[one.clone(), two.clone()]).unwrap();
    let again = assemble(&home, "dusty", &b, &[two.clone(), one.clone()]).unwrap();
    assert!(
        !first.reused && again.reused,
        "the order the KMs arrived in does not matter"
    );
    assert_eq!((&first.path, &first.hash), (&again.path, &again.hash));
    let just_one = assemble(&home, "dusty", &b, std::slice::from_ref(&one)).unwrap();
    assert!(!just_one.reused);
    assert_ne!(just_one.path, first.path);
    // another core with the same inputs is a different file
    assert_ne!(
        assemble(&home, "misty", &b, std::slice::from_ref(&one)).unwrap().path,
        just_one.path
    );
    // prune keeps what it is told to, and only this core's files (not a core whose name extends this one's)
    let longer = assemble(&home, "dusty-2", &b, std::slice::from_ref(&one)).unwrap();
    prune(&home, "dusty", Some(&just_one.path));
    assert!(just_one.path.is_file() && !first.path.is_file());
    assert!(longer.path.is_file(), "dusty-2's assembly belongs to another core");
    assert!(assemble(&home, "misty", &b, &[one]).unwrap().reused);
    // a changed base is a different hash
    let before = assembly_hash(&b, std::slice::from_ref(&two)).unwrap();
    let mut bytes = std::fs::read(&b).unwrap();
    bytes.extend([0u8; 32]);
    std::fs::write(&b, bytes).unwrap();
    assert_ne!(before, assembly_hash(&b, &[two]).unwrap());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn quantized_blocks_are_sized_and_copied_by_the_slot() {
    // Q8_0: 32 elements in 34 bytes; hidden 32 so a row is one block
    assert_eq!(type_layout(8), Some((32, 34)));
    let home = crate::testing::temp_home();
    let mk = |layer: u32, n: u64, fill: u8| {
        let mut v = vec![];
        for (suffix, dims) in [
            ("ffn_gate_exps.weight", vec![32, 2, n]),
            ("ffn_up_exps.weight", vec![32, 2, n]),
            ("ffn_down_exps.weight", vec![2, 32, n]),
        ] {
            let per = if suffix.starts_with("ffn_down") {
                32 * 34
            } else {
                2 * 34
            };
            v.push(T {
                name: format!("blk.{layer}.{suffix}"),
                dims,
                ty: 8,
                data: vec![fill; per * n as usize],
            });
        }
        v
    };
    let b = home.join("q.gguf");
    write(&b, &mk(0, 4, 1));
    let frag = bytes_of(&mk(0, 1, 9));
    // hidden/ffn are declared in the KM; they are checked against the base
    let wrong = km(&home.join("kms"), "ex.q", &[(0, [2, 2], frag.clone())]);
    let e = assemble(&home, "dusty", &b, &[wrong]).unwrap_err();
    assert!(e.message.contains("declares hidden 8"), "{}", e.message);
    let k = ExpertsKm::open(&sample::experts_with(
        &home.join("kms2"),
        "ex.q",
        "1.0.0",
        (32, 2, 4),
        &[(0, [2, 2], frag)],
    ))
    .unwrap();
    let out = assemble(&home, "dusty", &b, &[k]).unwrap();
    let (h, new) = (read_header(&b).unwrap(), std::fs::read(&out.path).unwrap());
    let gate = &h.tensors["blk.0.ffn_gate_exps.weight"];
    let stride = (gate.bytes / 4) as usize;
    let at = gate.offset as usize;
    assert!(new[at..at + 2 * stride].iter().all(|b| *b == 1));
    assert!(new[at + 2 * stride..at + 3 * stride].iter().all(|b| *b == 9));
    assert!(new[at + 3 * stride..at + 4 * stride].iter().all(|b| *b == 1));
    let _ = std::fs::remove_dir_all(&home);
}
