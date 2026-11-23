#define probeDims ivec3(probeExtent.xyz)
const vec3 probeMin = vec3(-2.7,0.3,-2.7);
const vec3 probeStep = vec3(0.9,0.85,0.9);
const vec3 axes[6] = vec3[](vec3(1,0,0),vec3(-1,0,0),vec3(0,1,0),
                           vec3(0,-1,0),vec3(0,0,1),vec3(0,0,-1));
struct Probe { vec4 lobe[6]; };

uint probeIndex(ivec3 i) {
    return uint(i.x+probeDims.x*(i.y+probeDims.y*i.z));
}
