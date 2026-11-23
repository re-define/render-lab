// Shared with RenderInstance::gpu_model and Targets::material_rows. Both strides use std430.
struct SceneModel {
    mat4 model;
    uvec4 data; // maximum scale, low/high transform revision, surface flags
};
struct SceneMaterial {
    vec4 color;
    vec4 params;
    uvec4 tex;
    uvec4 routing;
};
