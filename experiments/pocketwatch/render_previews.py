#!/usr/bin/env python3
"""Render the pocket-watch STLs to the on-theme PNGs used on the smol site.

These are the exact renders shown in the site's "Pocket Watch" section
(dark background + cyan, matching the OLED aesthetic).

Deps:   pip install trimesh matplotlib numpy manifold3d
Usage:  python3 render_previews.py [stl_dir] [out_dir]
        stl_dir default = this file's directory
        out_dir default = ../../site/renders

Body + assembly are rotated 180° about Z so the bail renders at the TOP
(i.e. the way the finished pocket watch hangs — the OLED is rotated 180° in
firmware to match). Regenerate after editing pocketwatch.py + re-exporting.
"""
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from mpl_toolkits.mplot3d.art3d import Poly3DCollection
import numpy as np, trimesh, sys, os

HERE = os.path.dirname(os.path.abspath(__file__))
STL_DIR = sys.argv[1] if len(sys.argv) > 1 else HERE
OUT_DIR = sys.argv[2] if len(sys.argv) > 2 else os.path.abspath(
    os.path.join(HERE, "..", "..", "site", "renders"))
os.makedirs(OUT_DIR, exist_ok=True)

BG = "#070b0c"
GLOW = np.array([125, 249, 255]) / 255.0
EDGE = "#0a3a40"
LIGHT = np.array([0.4, 0.5, 0.8]); LIGHT = LIGHT / np.linalg.norm(LIGHT)


def render(stl, outpat, views, flip=False):
    if not os.path.exists(stl):
        print("skip (missing)", os.path.basename(stl)); return
    m = trimesh.load(stl, force="mesh")
    m.apply_translation(-m.centroid)
    if flip:  # bail -> top, matching how the watch hangs
        m.apply_transform(trimesh.transformations.rotation_matrix(np.pi, [0, 0, 1]))
    tris = m.triangles
    shade = np.clip(m.face_normals @ LIGHT, 0, 1) * 0.75 + 0.25
    colors = np.zeros((len(tris), 4)); colors[:, :3] = GLOW * shade[:, None]; colors[:, 3] = 1
    r = float(np.max(np.linalg.norm(m.vertices, axis=1)))
    for label, (elev, azim) in views.items():
        fig = plt.figure(figsize=(5, 5), dpi=130); fig.patch.set_facecolor(BG)
        ax = fig.add_subplot(111, projection="3d"); ax.set_facecolor(BG)
        ax.add_collection3d(Poly3DCollection(tris, facecolors=colors, edgecolors=EDGE, linewidths=0.08))
        ax.set_xlim(-r, r); ax.set_ylim(-r, r); ax.set_zlim(-r, r)
        try:
            ax.set_box_aspect((1, 1, 1))
        except Exception:
            pass
        ax.view_init(elev=elev, azim=azim); ax.set_axis_off()
        fig.subplots_adjust(left=0, right=1, bottom=0, top=1)
        path = os.path.join(OUT_DIR, outpat % label)
        fig.savefig(path, facecolor=BG); plt.close(fig)
        print("wrote", os.path.relpath(path))


if __name__ == "__main__":
    j = lambda f: os.path.join(STL_DIR, f)
    render(j("pocketwatch_body.stl"), "pw_body_%s.png",
           {"front": (90, -90), "threequarter": (35, -60), "back": (-70, -90)}, flip=True)
    render(j("pocketwatch_lid.stl"), "pw_lid_%s.png", {"threequarter": (40, -55)})
    render(j("pocketwatch_crown.stl"), "pw_crown_%s.png", {"threequarter": (20, -50)})
    render(j("pocketwatch_assembly.stl"), "pw_assembly_%s.png", {"threequarter": (28, -58)}, flip=True)
    print("DONE")
