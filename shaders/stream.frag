#version 460
layout(location=0) in vec2 uv;
layout(location=0) out uvec2 visibility;
void main(){visibility=uvec2(1u,packUnorm2x16(uv));}
