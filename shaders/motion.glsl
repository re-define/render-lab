// std430 array stride is 24 bytes. Offsets use current-to-previous render pixels
// with neither frame's projection jitter. A negative depth marks invalid history.
struct MotionSample {
    vec2 offset;
    float previousDepth;
    uint previousTag;
    uint currentTag;
    uint flags;
};

const uint MOTION_TRACKED = 1u;
const uint MOTION_WIND = 2u;
const uint MOTION_REACTIVE = 4u;

const uint SURFACE_ORDINARY = 1u;
const uint SURFACE_CLUSTER_TRIANGLE = 2u;
const uint SURFACE_VOXEL = 3u;
