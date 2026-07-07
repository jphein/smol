import trimesh, numpy as np, os, shutil
from trimesh.intersections import slice_mesh_plane

WORK="/home/jp/Projects/smol/experiments/case-mod"
src=os.path.join(WORK,"original","esp32c6down3.stl")
outdir=os.path.join(WORK,"modified"); os.makedirs(outdir,exist_ok=True)
m=trimesh.load(src, force='mesh')
print("loaded bottom:", np.round(m.extents,2), "watertight", m.is_watertight)

axis=int(np.argmin(m.extents))                 # depth axis = smallest extent (Y here)
DELTA=6.5                                       # add depth for a 5mm 502030 + clearance
lo,hi=m.bounds[0][axis], m.bounds[1][axis]; mid=(lo+hi)/2.0
n=np.zeros(3); n[axis]=1.0
def origin_at(v):
    o=m.bounds[0].copy(); o[axis]=v; return o

# split the shell at mid
upper=slice_mesh_plane(m, plane_normal=n,  plane_origin=origin_at(mid), cap=True)
lower=slice_mesh_plane(m, plane_normal=-n, plane_origin=origin_at(mid), cap=True)

# take a thin wall slab around mid and stretch it into a collar (no section() needed)
half=0.6
slab=slice_mesh_plane(m,   plane_normal=n,  plane_origin=origin_at(mid-half), cap=True)
slab=slice_mesh_plane(slab, plane_normal=-n, plane_origin=origin_at(mid+half), cap=True)
print("slab extents", np.round(slab.extents,2), "wt", slab.is_watertight)
target=DELTA+0.4
cen=(slab.bounds[0][axis]+slab.bounds[1][axis])/2.0
factor=target/(slab.bounds[1][axis]-slab.bounds[0][axis])
v=slab.vertices.copy(); v[:,axis]=(v[:,axis]-cen)*factor+cen; slab.vertices=v
sh=np.zeros(3); sh[axis]=(mid-DELTA/2.0)-cen; slab.apply_translation(sh)   # fill gap w/ 0.2 overlaps

# drop the floor half by DELTA
t=np.zeros(3); t[axis]=-DELTA; lower.apply_translation(t)

result=trimesh.boolean.union([upper,slab,lower], engine='manifold')
print("RESULT watertight:", result.is_watertight, "extents:", np.round(result.extents,2),
      "vol(cm3):", round(result.volume/1000,2))
out=os.path.join(outdir,"esp32c6down3_battery.stl"); result.export(out); print("wrote", out)
shutil.copy(os.path.join(WORK,"original","esp32c6top3.stl"), os.path.join(outdir,"esp32c6top3.stl"))
print("copied lid unchanged -> modified/esp32c6top3.stl")
