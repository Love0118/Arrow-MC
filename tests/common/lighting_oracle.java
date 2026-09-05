import com.google.gson.*;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.*;
import net.minecraft.SharedConstants;
import net.minecraft.commands.Commands;
import net.minecraft.core.*;
import net.minecraft.server.*;
import net.minecraft.server.packs.repository.*;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.util.Util;
import net.minecraft.world.level.*;
import net.minecraft.world.level.block.*;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.*;
import net.minecraft.world.level.chunk.*;
import net.minecraft.world.level.lighting.*;
import net.minecraft.world.level.material.FluidState;

/** Independently authored synthetic public-API driver; never starts MinecraftServer. */
class LightingOracle {
    static { SharedConstants.tryDetectVersion(); Bootstrap.bootStrap(); }
    static final Gson JSON = new GsonBuilder().disableHtmlEscaping().setPrettyPrinting().create();
    static PalettedContainerFactory factory;
    static final BlockState AIR = Blocks.AIR.defaultBlockState();

    static ProtoChunk newChunk(int x,int z,int minY,int height) {
        return new ProtoChunk(new ChunkPos(x,z),UpgradeData.EMPTY,
            LevelHeightAccessor.create(minY,height),factory,null);
    }

    /** A real ProtoChunk collection for source/engine callers; missing blocks are bedrock. */
    static class FixtureWorld implements LightChunkGetter,BlockGetter {
        final int minY,height;
        final Map<ChunkPos,ProtoChunk> chunks=new HashMap<>();
        final List<String> notifications=new ArrayList<>();
        FixtureWorld(int minY,int height){this.minY=minY;this.height=height;}
        ProtoChunk add(int x,int z){ProtoChunk chunk=newChunk(x,z,minY,height);chunks.put(chunk.getPos(),chunk);return chunk;}
        public LightChunk getChunkForLighting(int x,int z){return chunks.get(new ChunkPos(x,z));}
        public BlockGetter getLevel(){return this;}
        public int getMinY(){return minY;}
        public int getHeight(){return height;}
        public BlockEntity getBlockEntity(BlockPos pos){return null;}
        public BlockState getBlockState(BlockPos pos){
            ProtoChunk chunk=chunks.get(new ChunkPos(pos.getX()>>4,pos.getZ()>>4));
            return chunk==null?Blocks.BEDROCK.defaultBlockState():chunk.getBlockState(pos);
        }
        public FluidState getFluidState(BlockPos pos){return getBlockState(pos).getFluidState();}
        public void onLightUpdate(LightLayer layer,SectionPos pos){notifications.add(layer+":"+pos.x()+","+pos.y()+","+pos.z());}
    }

    static void withWorldLoader(Runnable run) throws Exception {
        PackRepository packs=ServerPacksSource.createVanillaTrustedRepository();
        try(ExecutorService worker=Executors.newFixedThreadPool(2)){
            var init=new WorldLoader.InitConfig(new WorldLoader.PackConfig(packs,WorldDataConfiguration.DEFAULT,false,false),Commands.CommandSelection.DEDICATED,PermissionSet.ALL_PERMISSIONS);
            WorldLoader.<WorldDataConfiguration,Boolean>load(init,
                context->new WorldLoader.DataLoadOutput<>(context.dataConfiguration(),context.datapackDimensions()),
                (resources,managers,registries,config)->{
                    try(resources){managers.updateComponentsAndStaticRegistryTags();factory=PalettedContainerFactory.create(registries.compositeAccess());run.run();return true;}
                },worker,Runnable::run).join();
        }finally{Util.shutdownExecutors();}
    }

    static JsonObject snapshot(ChunkSkyLightSources sources){
        JsonObject out=new JsonObject();JsonArray columns=new JsonArray();
        for(int i=0;i<256;i++)columns.add(sources.getLowestSourceY(i&15,i>>4));
        out.add("columns",columns);out.addProperty("highest",sources.getHighestLowestSourceY());return out;
    }
    static JsonObject block(int x,int y,int z,BlockState state){
        JsonObject out=new JsonObject();out.addProperty("x",x);out.addProperty("y",y);out.addProperty("z",z);out.addProperty("state",Block.getId(state));return out;
    }
    static void put(ProtoChunk chunk,JsonArray placements,int x,int y,int z,BlockState state){chunk.setBlockState(new BlockPos(x,y,z),state,0);placements.add(block(x,y,z,state));}
    static void update(ProtoChunk chunk,ChunkSkyLightSources sources,JsonArray operations,int x,int y,int z,BlockState state){
        chunk.setBlockState(new BlockPos(x,y,z),state,0);JsonObject operation=block(x,y,z,state);operation.addProperty("changed",sources.update(chunk,x,y,z));operation.add("after",snapshot(sources));operations.add(operation);
    }
    static BlockState[] states(){
        return new BlockState[]{AIR,Blocks.STONE.defaultBlockState(),Blocks.WATER.defaultBlockState(),Blocks.GLASS.defaultBlockState(),Blocks.TINTED_GLASS.defaultBlockState(),Blocks.OAK_LEAVES.defaultBlockState(),
            Blocks.STONE_SLAB.defaultBlockState().setValue(BlockStateProperties.SLAB_TYPE,SlabType.BOTTOM),
            Blocks.STONE_SLAB.defaultBlockState().setValue(BlockStateProperties.SLAB_TYPE,SlabType.TOP),
            Blocks.STONE_STAIRS.defaultBlockState().setValue(BlockStateProperties.HALF,Half.BOTTOM),
            Blocks.STONE_STAIRS.defaultBlockState().setValue(BlockStateProperties.HALF,Half.TOP),
            Blocks.STONE_STAIRS.defaultBlockState().setValue(BlockStateProperties.STAIRS_SHAPE,StairsShape.INNER_LEFT),
            Blocks.STONE_STAIRS.defaultBlockState().setValue(BlockStateProperties.STAIRS_SHAPE,StairsShape.OUTER_RIGHT),
            Blocks.STONE_SLAB.defaultBlockState().setValue(BlockStateProperties.WATERLOGGED,true),
            Blocks.SNOW.defaultBlockState().setValue(BlockStateProperties.LAYERS,7),Blocks.CAVE_AIR.defaultBlockState(),Blocks.VOID_AIR.defaultBlockState()};
    }
    static JsonObject scenario(String name,int minY,int height,boolean populated){
        ProtoChunk chunk=newChunk(0,0,minY,height);ChunkSkyLightSources sources=new ChunkSkyLightSources(chunk);JsonArray initial=new JsonArray();JsonArray operations=new JsonArray();BlockState[] variants=states();
        if(populated){for(int i=0;i<256;i++){
            int y=minY+16+(i%4)*16;put(chunk,initial,i&15,y,i>>4,variants[i%variants.length]);
            put(chunk,initial,i&15,y-1,i>>4,variants[(i/variants.length)%variants.length]);
            if(i%3==0)put(chunk,initial,i&15,minY,i>>4,Blocks.STONE.defaultBlockState());
        }}
        sources.fillFrom(chunk);JsonObject row=new JsonObject();row.addProperty("name",name);row.addProperty("min_y",minY);row.addProperty("height",height);row.add("placements",initial);row.add("initial",snapshot(sources));
        // Every column exercises a source move and removal; roofs hit the top and bottom build edges.
        for(int i=0;i<256;i++)update(chunk,sources,operations,i&15,minY+height-1,i>>4,variants[(i+1)%variants.length]);
        for(int i=255;i>=0;i--)update(chunk,sources,operations,i&15,minY+height-1,i>>4,AIR);
        for(int i=0;i<256;i+=7){update(chunk,sources,operations,i&15,minY,i>>4,Blocks.STONE.defaultBlockState());update(chunk,sources,operations,i&15,minY,i>>4,AIR);}
        // Opposite faces at a section edge, then below-source no-ops and clearing the top blocker.
        for(int i=0;i<variants.length;i++){
            update(chunk,sources,operations,i,minY+32,15,variants[i]);update(chunk,sources,operations,i,minY+31,15,variants[(i+7)%variants.length]);
            update(chunk,sources,operations,i,minY+1,15,Blocks.WATER.defaultBlockState());update(chunk,sources,operations,i,minY+32,15,AIR);update(chunk,sources,operations,i,minY+31,15,AIR);
        }
        row.add("operations",operations);return row;
    }
    static JsonObject emptySectionBoundary(){
        int minY=-64,height=80;ProtoChunk chunk=newChunk(0,0,minY,height);ChunkSkyLightSources sources=new ChunkSkyLightSources(chunk);JsonArray placements=new JsonArray(),operations=new JsonArray();BlockState slab=Blocks.STONE_SLAB.defaultBlockState().setValue(BlockStateProperties.SLAB_TYPE,SlabType.BOTTOM);
        put(chunk,placements,3,minY+16,5,slab);sources.fillFrom(chunk);JsonObject row=new JsonObject();row.addProperty("name","all_air_section_under_slab_face");row.addProperty("min_y",minY);row.addProperty("height",height);row.add("placements",placements);row.add("initial",snapshot(sources));
        // Initial section skipping and an explicit update inspect different edges.
        update(chunk,sources,operations,3,minY+16,5,slab);update(chunk,sources,operations,3,minY+16,5,AIR);
        update(chunk,sources,operations,3,minY,5,slab);update(chunk,sources,operations,3,minY,5,AIR);
        update(chunk,sources,operations,3,minY+height-1,5,slab);update(chunk,sources,operations,3,minY+height-1,5,AIR);
        row.add("operations",operations);return row;
    }
    public static void main(String[] args)throws Exception{
        JsonObject report=new JsonObject();withWorldLoader(()->{
            report.addProperty("version",SharedConstants.getCurrentVersion().id());report.addProperty("data_version",SharedConstants.getCurrentVersion().dataVersion().version());report.addProperty("state_count",Block.BLOCK_STATE_REGISTRY.size());JsonArray scenarios=new JsonArray();
            scenarios.add(scenario("empty_overworld",-64,384,false));scenarios.add(scenario("mixed_faces_overworld",-64,384,true));scenarios.add(scenario("mixed_faces_short_dimension",0,80,true));scenarios.add(emptySectionBoundary());report.add("scenarios",scenarios);
        });Files.writeString(Path.of(args[0]),JSON.toJson(report)+"\n");System.out.println("Recorded source scenarios using actual ProtoChunk objects.");
    }
}
