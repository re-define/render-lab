#extension GL_EXT_buffer_reference2 : require
layout(buffer_reference,std430,buffer_reference_align=4) readonly buffer StreamPage {uint words[];};
layout(set=0,binding=PAGE_TABLE,std430) readonly buffer AddressTable {StreamPage pages[];};
uint wordAt(uint base,uint offset){return pages[base+offset/16384].words[offset%16384];}
