// Columns are the local-space change per screen pixel and the ray through
// the center of pixel (0,0). Both rasterizers evaluate at integer pixel IDs.
vec3 brickPixelRay(mat3 basis,uvec2 pixel) {
    precise vec3 dir=basis[2]+basis[0]*float(pixel.x)+basis[1]*float(pixel.y);
    return dir;
}
