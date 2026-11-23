layout(set=0, binding=0, std430) readonly buffer Vertices { vec4 vertices[]; };
layout(set=0, binding=1, std430) readonly buffer Faces { uvec4 faces[]; };
layout(set=0, binding=2, std430) readonly buffer Camera {
    mat4 vp;
    vec4 eye;
    vec2 screen;
    float width;
};
layout(set=0, binding=3, std430) readonly buffer InstanceTransform { mat4 model; };
layout(location=0) perprimitiveEXT flat out uvec2 id[];
uint groupIndex() { return gl_WorkGroupID.x + gl_WorkGroupID.y * 32768u; }
vec3 position(uint v) { return (model * vec4(vertices[2u * v].xyz,1)).xyz; }
vec2 unitOrZero(vec2 v) { return v * inversesqrt(max(dot(v, v), 1e-18)); }
bool front(vec3 a, vec3 b, vec3 c) { return dot(cross(b - a, c - a), eye.xyz - a) > 0.0; }
vec4 expandPixel(vec4 clip, vec2 unitPixel) {
    clip.xy += unitPixel * (2.0 * width / screen) * clip.w;
    clip.z += 0.000002 * clip.w;
    return clip;
}
