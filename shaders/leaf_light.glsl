// Two-sided diffuse reflection and filtered transmission across a thin leaf.
// The small angular wrap represents the spread of leaflet normals within a shoot.
vec3 leafResponse(float cosine) {
    float reflected = max((cosine + 0.25) / 1.25, 0.0);
    float transmitted = max((0.18 - cosine) / 1.18, 0.0);
    return vec3(0.86 * reflected) + vec3(0.38, 0.52, 0.29) * transmitted;
}
