import com.google.gson.*;
import java.nio.file.*;
import java.util.*;
import net.minecraft.core.*;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.state.*;
import net.minecraft.world.level.block.state.properties.*;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.lighting.*;

/** Independent API scenarios over actual ProtoChunks; no game server startup. */
class LightingSkyOracle {
    static final HexFormat HEX=HexFormat.of();
    static JsonObject state(LevelLightEngine engine,LightingOracle.FixtureWorld world,String label){
        JsonObject row=new JsonObject();row.addProperty("label",label);JsonArray layers=new JsonArray();
        var sky=engine.getLayerListener(LightLayer.SKY);
        for(int z=-1;z<=3;z++)for(int x=-1;x<=3;x++)for(int y=-1;y<=6;y++){
            DataLayer layer=sky.getDataLayerData(SectionPos.of(x,y,z));
            if(layer!=null){JsonObject item=new JsonObject();item.addProperty("x",x);item.addProperty("y",y);item.addProperty("z",z);item.addProperty("empty",layer.isEmpty());item.addProperty("uniform",layer.isDefinitelyHomogenous());item.addProperty("bytes",HEX.formatHex(layer.copy().getData()));layers.add(item);}
        }
        row.add("layers",layers);JsonArray probes=new JsonArray();
        for(int[] pos:new int[][]{{0,-16,0},{0,96,0},{-1,16,0},{16,16,16},{15,48,15},{16,32,15},{32,63,15},{47,0,47}}){JsonArray probe=new JsonArray();for(int v:pos)probe.add(v);probe.add(sky.getLightValue(new BlockPos(pos[0],pos[1],pos[2])));probes.add(probe);}row.add("probes",probes);
        TreeSet<String> notices=new TreeSet<>(world.notifications);row.add("notifications",LightingOracle.JSON.toJsonTree(notices));world.notifications.clear();return row;
    }
    static void put(LightingOracle.FixtureWorld world,JsonArray placements,int x,int y,int z,BlockState value){
        ProtoChunk chunk=world.chunks.get(new ChunkPos(x>>4,z>>4));chunk.setBlockState(new BlockPos(x,y,z),value,0);placements.add(LightingOracle.block(x,y,z,value));
    }
    static JsonObject scenario(String name,boolean gap){
        var world=new LightingOracle.FixtureWorld(0,96);for(int z=0;z<3;z++)for(int x=0;x<3;x++)world.add(x,z);
        JsonArray placements=new JsonArray();
        // Bottom and high islands leave empty support gaps; the neighboring
        // middle island requires lateral sky propagation through that gap.
        for(int z=0;z<48;z++)for(int x=0;x<48;x++)if((x+z)%7==0)put(world,placements,x,0,z,Blocks.STONE.defaultBlockState());
        if(gap){for(int z=0;z<16;z++)for(int x=0;x<16;x++)put(world,placements,x,80,z,Blocks.STONE.defaultBlockState());for(int z=16;z<32;z++)for(int x=16;x<32;x++)put(world,placements,x,32,z,Blocks.STONE.defaultBlockState());}
        else{
            for(int z=8;z<25;z++)for(int x=8;x<25;x++)put(world,placements,x,47,z,(x==15||z==16)?Blocks.WATER.defaultBlockState():Blocks.STONE.defaultBlockState());
            var variants=LightingOracle.states();for(int i=0;i<variants.length;i++)put(world,placements,15,32+i,15,variants[i]);
        }
        var engine=new LevelLightEngine(world,false,true);JsonArray states=new JsonArray();
        for(int z=0;z<3;z++)for(int x=0;x<3;x++){
            ProtoChunk chunk=world.chunks.get(new ChunkPos(x,z));chunk.initializeLightSources();
            for(int y=0;y<6;y++)if(!chunk.getSection(y).hasOnlyAir())engine.updateSectionStatus(SectionPos.of(x,y,z),false);
        }
        engine.runLightUpdates();states.add(state(engine,world,"sections"));
        for(int z=0;z<3;z++)for(int x=0;x<3;x++)engine.setLightEnabled(new ChunkPos(x,z),true);
        engine.runLightUpdates();states.add(state(engine,world,"enabled"));
        for(int z=0;z<3;z++)for(int x=0;x<3;x++)engine.propagateLightSources(new ChunkPos(x,z));
        engine.runLightUpdates();states.add(state(engine,world,"sources"));
        JsonArray updates=new JsonArray();BlockState[] changes={Blocks.STONE.defaultBlockState(),Blocks.AIR.defaultBlockState(),Blocks.TINTED_GLASS.defaultBlockState(),Blocks.GLASS.defaultBlockState(),Blocks.WATER.defaultBlockState(),Blocks.AIR.defaultBlockState()};
        for(int i=0;i<changes.length;i++){
            int x=i%2==0?15:16,y=47,z=15;
            ProtoChunk chunk=world.chunks.get(new ChunkPos(x>>4,z>>4));chunk.setBlockState(new BlockPos(x,y,z),changes[i],0);chunk.getSkyLightSources().update(chunk,x&15,y,z&15);
            engine.checkBlock(new BlockPos(x,y,z));engine.runLightUpdates();JsonObject update=LightingOracle.block(x,y,z,changes[i]);update.add("after",state(engine,world,"change-"+i));updates.add(update);
        }
        engine.setLightEnabled(new ChunkPos(0,0),false);engine.checkBlock(new BlockPos(15,47,15));engine.runLightUpdates();states.add(state(engine,world,"disabled"));
        engine.setLightEnabled(new ChunkPos(0,0),true);engine.propagateLightSources(new ChunkPos(0,0));engine.runLightUpdates();states.add(state(engine,world,"reenabled"));
        JsonObject result=new JsonObject();result.addProperty("name",name);result.addProperty("min_y",0);result.addProperty("height",96);result.add("placements",placements);result.add("states",states);result.add("updates",updates);return result;
    }
    public static void main(String[] args)throws Exception{
        JsonObject report=new JsonObject();LightingOracle.withWorldLoader(()->{JsonArray scenarios=new JsonArray();scenarios.add(scenario("roof_faces",false));scenarios.add(scenario("empty_section_bridge",true));report.add("scenarios",scenarios);});
        Files.writeString(Path.of(args[0]),LightingOracle.JSON.toJson(report)+"\n");System.out.println("Recorded two multi-chunk sky-light scenarios.");
    }
}
