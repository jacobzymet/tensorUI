/* ================================================================
 * Dotted thought-orbs — adapted from thinking-orbs
 *   https://github.com/Jakubantalik/thinking-orbs
 *   https://orbs.jakubantalik.com
 *
 * Copyright (c) 2026 Jakub Antalik
 * SPDX-License-Identifier: MIT
 *
 * Full MIT license text: THIRD_PARTY_NOTICES.md
 *
 * Modes carried over:
 *   - orbits ("working") — header / chat CTAs
 *   - globe  ("searching") — starting & downloading status spinner
 *
 * Shipped 64px / 20px / 14px tunings are pre-resolved into constants
 * (upstream preset machinery scales counts and radii; those multipliers
 * are already applied below). Grayscale ink is tinted toward the cobalt
 * accent so the mark belongs to this design system.
 *
 * Shared by the control panel and the chat page, so it is served as its
 * own asset rather than inlined twice.
 * ================================================================ */

const ORB_PRESETS = {
  orbits: {
    64: {
      speed: 1.885,
      opts: { orbitN: 12, ghostN: 40, ghostR: 0.9, ghostA: 0.5, particles: 3, partR: 1.2, partRDepth: 1.6, rsPow: 0.6, rMin: 0.3 },
    },
    20: {
      speed: 3.9,
      opts: { orbitN: 3, ghostN: 10, ghostR: 2.16, ghostA: 0.5, particles: 3, partR: 2.88, partRDepth: 3.84, rsPow: 0.6, rMin: 0.3 },
    },
  },
  // searching — lat/long field with a scan meridian (size 20 / 14 tunings)
  globe: {
    20: {
      speed: 2.665,
      opts: {
        latRings: 6,
        lonDensity: 14,
        rBase: 1.05,
        rDepth: 2.975,
        rBoost: 1.75,
        inkFar: 0.62,
        inkSpan: 0.54,
        rsPow: 0.6,
        rMin: 0.3,
        scanMul: 4.335,
        dimBase: 0.45,
      },
    },
    14: {
      speed: 2.9,
      opts: {
        latRings: 5,
        lonDensity: 12,
        rBase: 1.2,
        rDepth: 3.4,
        rBoost: 2.0,
        inkFar: 0.62,
        inkSpan: 0.54,
        rsPow: 0.55,
        rMin: 0.35,
        scanMul: 4.5,
        dimBase: 0.42,
      },
    },
    12: {
      speed: 3.0,
      opts: {
        latRings: 4,
        lonDensity: 10,
        rBase: 1.35,
        rDepth: 3.6,
        rBoost: 2.1,
        inkFar: 0.62,
        inkSpan: 0.54,
        rsPow: 0.5,
        rMin: 0.4,
        scanMul: 4.6,
        dimBase: 0.4,
      },
    },
  },
};
/* Multiplied into the grayscale ink value; max channel 1 keeps highlights bright.
 * Tuned to the cobalt accent (oklch(70% 0.15 255)) so the mark reads as part of
 * the accent family rather than a neutral gray sphere. */
const ORB_TINT = [0.62, 0.79, 1];

/** Deterministic hash in [0, 1). */
function orbHash(a, b) {
  const h = Math.sin(a * 12.9898 + b * 78.233) * 43758.5453;
  return h - Math.floor(h);
}

/** Shortest signed angular distance, wrapped to (-π, π]. */
function orbAngleDelta(a, b) {
  return Math.atan2(Math.sin(a - b), Math.cos(a - b));
}

/** Spin + tilt + orthographic projection. */
function orbProjector(yaw, tilt, cx, cy, scale) {
  const st = Math.sin(tilt), ct = Math.cos(tilt);
  const sy = Math.sin(yaw), cyw = Math.cos(yaw);
  return (x, y, z) => {
    const x1 = x * cyw + z * sy;
    const z1 = -x * sy + z * cyw;
    const y1 = y * ct - z1 * st;
    const z2 = y * st + z1 * ct;
    return [cx + x1 * scale, cy - y1 * scale, z2];
  };
}

/** Dot radii were tuned for a 300pt frame; sub-linear scaling keeps small orbs legible. */
function orbRadiusScale(size, pow) {
  return Math.pow(size / 300, pow);
}

/** z-sort far→near, then fill matte dots. Ink is mirrored for the dark substrate. */
function orbPaint(ctx, dots, rMin, ink) {
  // ink: 'auto' (theme), 'bright' (white-ish on accent/dark), 'dark' (ink on paper)
  let light =
    typeof document !== 'undefined' &&
    document.documentElement &&
    document.documentElement.dataset.theme === 'light';
  if (ink === 'bright') light = false;
  else if (ink === 'dark') light = true;
  const tint = ink === 'bright' ? [1, 1, 1] : ORB_TINT;
  dots.sort((a, b) => a.z - b.z);
  for (const d of dots) {
    const alpha = d.a === undefined ? 1 : d.a;
    if (alpha < 0.02) continue;
    const w = Math.min(1, Math.max(0, d.white));
    const g = (light ? w : 1 - w) * 255;
    const r = Math.round(g * tint[0]);
    const gr = Math.round(g * tint[1]);
    const b = Math.round(g * tint[2]);
    ctx.fillStyle = 'rgba(' + r + ',' + gr + ',' + b + ',' + alpha + ')';
    ctx.beginPath();
    ctx.arc(d.x, d.y, Math.max(rMin, d.r), 0, Math.PI * 2);
    ctx.fill();
  }
}

/** Particles on tilted orbits: a ghost path per orbit plus the ones doing the work. */
function drawOrbits(ctx, size, t, o, ink) {
  const cx = size / 2;
  const cy = size / 2;
  const R = (size / 2) * 0.82;
  const project = orbProjector(t * 0.12, 0.3, cx, cy, 1);
  const rs = orbRadiusScale(size, o.rsPow);
  const dots = [];

  for (let orb = 0; orb < o.orbitN; orb++) {
    const h1 = orbHash(orb, 1.7);
    const h2 = orbHash(orb, 5.2);
    const h3 = orbHash(orb, 8.9);
    const ro = R * (0.45 + 0.52 * h1);
    const th = h1 * 2 * Math.PI;
    const phi = Math.acos(2 * h2 - 1);
    // orbit plane basis (u, v perpendicular to normal n)
    const nx = Math.sin(phi) * Math.cos(th);
    const ny = Math.cos(phi);
    const nz = Math.sin(phi) * Math.sin(th);
    let ux = -ny, uy = nx;
    const uz = 0;
    const ul = Math.max(1e-6, Math.sqrt(ux * ux + uy * uy));
    ux /= ul;
    uy /= ul;
    const vx = ny * uz - nz * uy;
    const vy = nz * ux - nx * uz;
    const vz = nx * uy - ny * ux;
    const speed = (0.25 + 0.55 * h3) * (h3 > 0.5 ? 1 : -1);

    for (let k = 0; k < o.ghostN; k++) {
      const a = (k / o.ghostN) * 2 * Math.PI;
      const ca = Math.cos(a), sa = Math.sin(a);
      const p = project((ux * ca + vx * sa) * ro, (uy * ca + vy * sa) * ro, (uz * ca + vz * sa) * ro);
      const depth = (p[2] / ro + 1) / 2;
      dots.push({ x: p[0], y: p[1], z: p[2], r: o.ghostR * rs, white: 0.72, a: o.ghostA * (0.4 + 0.6 * depth) });
    }
    for (let m = 0; m < o.particles; m++) {
      const a = t * speed + (m / o.particles) * 2 * Math.PI + h2 * 6;
      const ca = Math.cos(a), sa = Math.sin(a);
      const p = project((ux * ca + vx * sa) * ro, (uy * ca + vy * sa) * ro, (uz * ca + vz * sa) * ro);
      const depth = (p[2] / ro + 1) / 2;
      dots.push({ x: p[0], y: p[1], z: p[2], r: (o.partR + o.partRDepth * depth) * rs, white: 0.3 - 0.22 * depth });
    }
  }
  orbPaint(ctx, dots, o.rMin, ink);
}

/** Lat/long field with a scan meridian sweeping — searching. */
function drawGlobe(ctx, size, t, o, ink) {
  const spin = 0.5;
  const cx = size / 2;
  const cy = size / 2;
  const radius = (size / 2) * 0.82;
  const tilt = 0.4 + 0.06 * Math.sin(t * 0.35);
  const pt = orbProjector(t * spin, tilt, cx, cy, radius);
  const scan = t * (spin + (1.7 - spin) * (o.scanMul === undefined ? 1 : o.scanMul));
  const rs = orbRadiusScale(size, o.rsPow === undefined ? 0.6 : o.rsPow);
  const dimBase = o.dimBase === undefined ? 1 : o.dimBase;
  const latRings = o.latRings === undefined ? 17 : o.latRings;
  const lonDensity = o.lonDensity === undefined ? 44 : o.lonDensity;
  const rBase = o.rBase === undefined ? 0.6 : o.rBase;
  const rDepth = o.rDepth === undefined ? 1.7 : o.rDepth;
  const rBoost = o.rBoost === undefined ? 1 : o.rBoost;
  const inkFar = o.inkFar === undefined ? 0.62 : o.inkFar;
  const inkSpan = o.inkSpan === undefined ? 0.54 : o.inkSpan;

  const dots = [];
  for (let li = 0; li <= latRings; li++) {
    const lat = -Math.PI / 2 + (li / latRings) * Math.PI;
    const cosLat = Math.cos(lat);
    const sinLat = Math.sin(lat);
    const lonCount = Math.max(1, Math.round(Math.abs(cosLat) * lonDensity));
    for (let lj = 0; lj < lonCount; lj++) {
      const lon = (lj / lonCount) * 2 * Math.PI;
      const p = pt(cosLat * Math.cos(lon), sinLat, cosLat * Math.sin(lon));
      const depth = (p[2] + 1) / 2;
      const d = orbAngleDelta(lon + t * spin, scan);
      const boost = Math.exp(-(d * d) / 0.18) * Math.max(0, p[2]);
      dots.push({
        x: p[0],
        y: p[1],
        z: p[2],
        r: (rBase + rDepth * depth + rBoost * boost) * rs,
        white: inkFar - inkSpan * depth,
        a: dimBase + (1 - dimBase) * Math.min(1, boost),
      });
    }
  }
  orbPaint(ctx, dots, o.rMin === undefined ? 0.3 : o.rMin, ink);
}

/**
 * Drive an orb on `canvas`. Returns { play, pause, stop }.
 *
 * options.mode: 'orbits' (default) | 'globe'
 * options.ink: 'auto' (theme) | 'bright' (white on accent buttons) | 'dark'
 * With `autoplay: false` the orb paints one static frame and waits — used
 * for the header mark, which only spins while hovered so the control panel
 * chrome stays calm. Pauses on hidden tabs; reduced-motion users always get
 * the single static frame.
 */
function mountOrb(canvas, size, options) {
  const settings = options || {};
  const autoplay = settings.autoplay !== false;
  const mode = settings.mode || 'orbits';
  const ink = settings.ink || 'auto';
  const modePresets = ORB_PRESETS[mode] || ORB_PRESETS.orbits;
  const preset = modePresets[size] || modePresets[20] || ORB_PRESETS.orbits[20];
  const draw = mode === 'globe' ? drawGlobe : drawOrbits;
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  canvas.width = Math.round(size * dpr);
  canvas.height = Math.round(size * dpr);
  canvas.style.width = size + 'px';
  canvas.style.height = size + 'px';
  const ctx = canvas.getContext('2d');
  const noop = { play() {}, pause() {}, stop() {} };
  if (!ctx) return noop;

  const frame = (tSec) => {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, size, size);
    draw(ctx, size, tSec, preset.opts, ink);
  };
  const liveFrame = () => frame((performance.now() / 1000) * preset.speed);

  // Paint one frame up front so the orb is never blank while waiting for
  // the first animation frame (rAF is throttled on hidden/offscreen tabs).
  liveFrame();

  if (window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
    frame(0.6);
    return noop;
  }

  let raf = 0;
  let running = false;
  let desired = autoplay;
  let stopped = false;
  const loop = () => {
    liveFrame();
    if (running) raf = requestAnimationFrame(loop);
  };
  const startLoop = () => {
    if (running || stopped || document.visibilityState === 'hidden') return;
    running = true;
    raf = requestAnimationFrame(loop);
  };
  const stopLoop = () => {
    running = false;
    cancelAnimationFrame(raf);
  };
  const play = () => {
    if (stopped) return;
    desired = true;
    startLoop();
  };
  const pause = () => {
    desired = false;
    stopLoop();
  };
  const onVisibility = () => {
    if (document.visibilityState === 'hidden') stopLoop();
    else if (desired && !stopped) startLoop();
  };
  document.addEventListener('visibilitychange', onVisibility);
  if (autoplay) play();

  return {
    play,
    pause,
    stop() {
      stopped = true;
      pause();
      document.removeEventListener('visibilitychange', onVisibility);
    },
  };
}
