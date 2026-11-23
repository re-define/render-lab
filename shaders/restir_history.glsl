#ifdef WRITE_HISTORY
layout(set=0,binding=RESTIR_HISTORY,std430) buffer History {
#else
layout(set=0,binding=RESTIR_HISTORY,std430) readonly buffer History {
#endif
    uvec4 frame;
    Previous previous[];
};
