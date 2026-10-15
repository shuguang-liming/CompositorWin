//! Rust translation of Rendering/ContentFill.c's coherent randomized patch search.
fn random(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    *state
}
fn patch_score(
    pixels: &[u8],
    known: &[bool],
    w: usize,
    h: usize,
    p: usize,
    q: usize,
    radius: i64,
) -> f64 {
    let px = (p % w) as i64;
    let py = (p / w) as i64;
    let qx = (q % w) as i64;
    let qy = (q / w) as i64;
    let mut sum = 0.;
    let mut count = 0;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let x = px + dx;
            let y = py + dy;
            let sx = qx + dx;
            let sy = qy + dy;
            if x < 0
                || y < 0
                || x >= w as i64
                || y >= h as i64
                || sx < 0
                || sy < 0
                || sx >= w as i64
                || sy >= h as i64
                || !known[y as usize * w + x as usize]
            {
                continue;
            }
            let a = (y as usize * w + x as usize) * 4;
            let b = (sy as usize * w + sx as usize) * 4;
            for c in 0..4 {
                let d = pixels[a + c] as f64 - pixels[b + c] as f64;
                sum += d * d;
            }
            count += 1;
        }
    }
    if count > 0 {
        sum / count as f64
    } else {
        f64::MAX
    }
}
fn neighbors(p: usize, w: usize, h: usize) -> [Option<usize>; 4] {
    let x = p % w;
    let y = p / w;
    [
        p.checked_sub(1).filter(|_| x > 0),
        (x + 1 < w).then_some(p + 1),
        p.checked_sub(w).filter(|_| y > 0),
        (y + 1 < h).then_some(p + w),
    ]
}

/// Premultiplied RGBA8. Returns false when there is no complete opaque donor patch.
pub fn content_fill(pixels: &mut [u8], mask: &[u8], w: usize, h: usize) -> bool {
    assert_eq!(pixels.len(), w * h * 4);
    assert_eq!(mask.len(), w * h);
    let n = w * h;
    let mut known = vec![false; n];
    let mut target = vec![false; n];
    let mut valid = vec![false; n];
    let mut queued = vec![false; n];
    let mut chosen = vec![None; n];
    let mut donors = Vec::new();
    let mut queue = Vec::new();
    let mut scan = 0;
    let radius = if w >= 5 && h >= 5 { 2 } else { 0 };
    let mut missing = 0;
    for p in 0..n {
        target[p] = mask[p] != 0;
        known[p] = !target[p] && pixels[p * 4 + 3] == 255;
        missing += usize::from(target[p]);
    }
    if missing == 0 {
        return true;
    }
    for y in 0..h {
        for x in 0..w {
            let p = y * w + x;
            if !known[p] {
                continue;
            }
            let mut ok = true;
            'patch: for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let sx = x as i64 + dx;
                    let sy = y as i64 + dy;
                    if sx < 0
                        || sy < 0
                        || sx >= w as i64
                        || sy >= h as i64
                        || !known[sy as usize * w + sx as usize]
                    {
                        ok = false;
                        break 'patch;
                    }
                }
            }
            if ok {
                valid[p] = true;
                donors.push(p);
            }
        }
    }
    if donors.is_empty() {
        return false;
    }
    for p in 0..n {
        if target[p] && neighbors(p, w, h).iter().flatten().any(|&p| known[p]) {
            queue.push(p);
            queued[p] = true;
        }
    }
    let mut seed = 0x6d2b79f5;
    let mut head = 0;
    loop {
        while head < queue.len() {
            let p = queue[head];
            head += 1;
            let nearby = neighbors(p, w, h);
            let mut best = None;
            let mut score = f64::MAX;
            for k in 0..28 {
                let q = if k < 4 {
                    nearby
                        .get(k)
                        .copied()
                        .flatten()
                        .map(|t| chosen[t].unwrap_or(t) as i64 + p as i64 - t as i64)
                } else {
                    Some(donors[random(&mut seed) as usize % donors.len()] as i64)
                };
                let Some(q) = q.filter(|&q| q >= 0 && (q as usize) < n && valid[q as usize]) else {
                    continue;
                };
                let q = q as usize;
                let s = patch_score(pixels, &known, w, h, p, q, radius);
                if best.is_none() || s < score {
                    score = s;
                    best = Some(q);
                }
            }
            let mut best = best.unwrap_or(donors[0]);
            let mut r = 64;
            while r >= 1 {
                let qx = (best % w) as i64 + (random(&mut seed) % (2 * r + 1)) as i64 - r as i64;
                let qy = (best / w) as i64 + (random(&mut seed) % (2 * r + 1)) as i64 - r as i64;
                if qx >= 0
                    && qy >= 0
                    && qx < w as i64
                    && qy < h as i64
                    && valid[qy as usize * w + qx as usize]
                {
                    let q = qy as usize * w + qx as usize;
                    let s = patch_score(pixels, &known, w, h, p, q, radius);
                    if s < score {
                        score = s;
                        best = q;
                    }
                }
                r /= 2;
            }
            let color: [u8; 4] = pixels[best * 4..best * 4 + 4].try_into().unwrap();
            pixels[p * 4..p * 4 + 4].copy_from_slice(&color);
            known[p] = true;
            chosen[p] = Some(best);
            for q in nearby.into_iter().flatten() {
                if target[q] && !known[q] && !queued[q] {
                    queue.push(q);
                    queued[q] = true;
                }
            }
        }
        while scan < n && (!target[scan] || known[scan]) {
            scan += 1;
        }
        if scan >= n {
            break;
        }
        queue.push(scan);
        queued[scan] = true;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fills_even_when_the_hole_only_touches_transparency() {
        let mut p = vec![0; 15 * 15 * 4];
        for y in 0..7 {
            for x in 0..7 {
                p[(y * 15 + x) * 4..(y * 15 + x) * 4 + 4].copy_from_slice(&[80, 60, 40, 255]);
            }
        }
        let mut mask = vec![0; 225];
        mask[12 * 15 + 12] = 255;
        assert!(content_fill(&mut p, &mask, 15, 15));
        assert_eq!(
            &p[(12 * 15 + 12) * 4..(12 * 15 + 12) * 4 + 4],
            &[80, 60, 40, 255]
        );
        assert_eq!(&p[(8 * 15 + 8) * 4..(8 * 15 + 8) * 4 + 4], &[0; 4]);
    }
    #[test]
    fn no_opaque_source_reports_failure() {
        assert!(!content_fill(&mut [0; 25 * 4], &[255; 25], 5, 5));
    }
}
