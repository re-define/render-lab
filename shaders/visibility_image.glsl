#extension GL_EXT_samplerless_texture_functions : require
layout(set=0,binding=0) uniform utexture2D visibilityImage;
uvec2 visibilityAt(uvec2 pixel) {
    return texelFetch(visibilityImage,ivec2(pixel),0).xy;
}
uvec2 visibilityAt(uint index,uvec2 screen) {
    return visibilityAt(uvec2(index%screen.x,index/screen.x));
}
