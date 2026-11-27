#version 460
layout(location=0) in vec2 uv;
layout(location=1) in float light;
layout(location=2) flat in uint material;
layout(location=0) out uvec2 visibility;
void main(){
    visibility=uvec2(material|(uint(clamp(light,0.,1.)*255.)<<8),packHalf2x16(uv));
}
