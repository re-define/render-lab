// Layouts shared by the trace, cache update, probe integration and resolve passes.
const uint INVALID=0xffffffffu;
const uint RAYS=8u;
const uint TILE=8u;
// Keeps uint sums within range even if every ray lands in one cell at high resolution.
const uint SAMPLE_CAP=65536u;
struct Cell {
    uvec4 tag; // hashed surface, last frame
    vec4 firstDirect;
    vec4 secondDirect;
    vec4 indirect;
    Reservoir firstLamp;
    Reservoir secondLamp;
    uvec4 pending; // second-hit lighting transported to the first hit next frame
};
struct Samples { uvec4 first; uvec4 second; uvec4 representative; };
struct Path {
    uvec4 address; // first cell, second cell, anchor pixel, padding
    vec4 first;    // uncached outgoing direct light, including emission
    vec4 second;
    Reservoir firstLamp;
    Reservoir secondLamp;
    vec4 weight;   // diffuse reflectance at first hit divided by continuation probability
};
struct Probe { Surface anchor; vec4 light; };
vec4 accumulate(vec4 last,vec3 sum,float count,float cap) {
    if(count==0.) return last;
    float n=min(last.w,cap);
    return vec4((last.rgb*n+sum)/(n+count),min(n+count,cap));
}
uvec3 encodeLight(vec3 v) { return uvec3(round(clamp(v,vec3(0),vec3(48))*512.)); }
vec3 decodeLight(uvec3 v) { return vec3(v)/512.; }
