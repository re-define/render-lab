struct Triangle { vec4 point[3]; vec4 normal[3]; vec4 color; uvec4 material; };
struct RayNode { vec3 low; uint first; vec3 high; uint count; };
struct LightingPixel { vec4 position; vec4 normal; vec4 direct; };
const float TAU = 6.28318531;

#ifdef LIGHTING_CONFIG
layout(set=0,binding=LIGHTING_CONFIG,std430) readonly buffer LightingConfig {
    mat4 lightVP;
    vec4 lightPosition;
    vec4 lightRight;
    vec4 lightUp;
    vec4 lightForward;
    vec4 lens; // near, far, tan(fov/2), world bias
    uvec4 modes; // experiment, GI, shadow, output
    uvec4 probeExtent;
};
#endif

uint hash(uint x) {
    x ^= x >> 16;
    x *= 0x7feb352du;
    x ^= x >> 15;
    x *= 0x846ca68bu;
    return x ^ (x >> 16);
}
float random(inout uint seed) {
    seed = hash(seed);
    return float(seed >> 8) * (1.0 / 16777216.0);
}
vec2 disk(int i, int count, float angle) {
    float a = float(i) * 2.39996323 + angle;
    return sqrt((float(i) + 0.5) / float(count)) * vec2(cos(a), sin(a));
}
vec3 cosineDirection(vec3 n, inout uint seed) {
    float r = sqrt(random(seed));
    float a = TAU * random(seed);
    vec3 tangent = normalize(cross(abs(n.y) < 0.98 ? vec3(0,1,0) : vec3(1,0,0), n));
    return r * (cos(a) * tangent + sin(a) * cross(n,tangent)) + sqrt(1.0-r*r) * n;
}
vec3 triangleNormal(Triangle t, vec2 uv) {
    vec3 n = t.normal[0].xyz * (1.0-uv.x-uv.y) + t.normal[1].xyz * uv.x + t.normal[2].xyz * uv.y;
    return normalize(dot(n,n) > 1e-10 ? n : cross(t.point[1].xyz-t.point[0].xyz, t.point[2].xyz-t.point[0].xyz));
}
vec2 triangleUV(Triangle t, vec2 bary) {
    vec3 b=vec3(1.-bary.x-bary.y,bary);
    return vec2(dot(b,vec3(t.point[0].w,t.point[1].w,t.point[2].w)),
                dot(b,vec3(t.normal[0].w,t.normal[1].w,t.normal[2].w)));
}
