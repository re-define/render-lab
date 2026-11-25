// Scene effects use RGB in both variants. Packed standalone studies keep their display ABI.
#ifdef HDR
layout(set=0,binding=SCENE_COLOR_BINDING,std430) buffer SceneColor { vec4 pixels[]; };
vec3 loadSceneColor(uint index) { return pixels[index].rgb; }
void storeSceneColor(uint index, vec3 rgb) { pixels[index]=vec4(rgb,1.); }
vec3 authoredColor(vec3 srgb) { return pow(max(srgb,0.),vec3(2.2)); }
#else
layout(set=0,binding=SCENE_COLOR_BINDING,std430) buffer SceneColor { uint pixels[]; };
vec3 loadSceneColor(uint index) { return unpackUnorm4x8(pixels[index]).bgr; }
void storeSceneColor(uint index, vec3 rgb) { pixels[index]=packUnorm4x8(vec4(rgb.bgr,1.)); }
vec3 authoredColor(vec3 srgb) { return srgb; }
#endif
