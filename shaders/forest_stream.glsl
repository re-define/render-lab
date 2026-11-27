const uint NONE=0xffffffffu;
struct CanopyGroup {vec4 sphere;uint errorBits,unit,root,parents,count,pad0,pad1,pad2;};
struct Unit {uint level,first,offset,pad;};
struct Frame {mat4 vp;vec4 eye;uvec4 size;}; // width, height, unit groups, chunks
