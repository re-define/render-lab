// Each unit can be a tile or an atomic DAG sibling group. A short unique-ID
// list avoids processing the full score table except on feedback overflow.
layout(set=0,binding=STREAM_REQUEST_BINDING,std430) buffer StreamFeedback {
    uint streamUnits,streamCount;
    uint streamData[];
};
void requestStreamUnit(uint unit,uint level,uint priority){
    if(unit>=streamUnits || level==0u)return;
    uint cap=min(streamUnits,512u);
    uint score=(min(level,255u)<<24u)|min(priority,0xffffffu);
    if(atomicMax(streamData[cap+unit],score)==0u){
        uint pos=atomicAdd(streamCount,1u);
        if(pos<cap)streamData[pos]=unit;
    }
}
