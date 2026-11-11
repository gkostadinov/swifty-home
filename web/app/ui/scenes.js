import {ROOM_ORDER,SCENEDEFS,slug,roomByName,capOf,D,TGT,BULBID} from '../core/store.js';
import {bulbById,roomBulbs,pickedRoom} from '../pages/home.js';
import {PENDING,run} from '../core/actions.js';
import {toast} from './toast.js';
import {ACT} from '../core/catalog.js';
import {dispatch,doNow} from '../core/guards.js';
import {SHEET,openSheet,row,roomOf} from './sheets.js';

const SCENE_TARGET={Evening:'evening',Focus:'focus','Lights off':'all_off'};
/* the house's scenes, as the hub keeps them: one level for every light, and the lights that have their own */
const SCENE_IDS={Movie:'movie',Evening:'evening',Focus:'focus','Lights off':'all_off'};
function sceneLevels(){const out={};for(const [n,id] of Object.entries(SCENE_IDS)){const s=SCENEDEFS.find(x=>x.id===id);if(s)out[n]={percent:+s.percent||0,own:Object.fromEntries((s.overrides||[]).map(o=>[o.target,+o.percent||0]))}}return out}
/* Movie dims only the TV's room, the same choice the server makes, so only that room's lights can say it is on */
const movieRoom=()=>roomOf(TGT.tv)||ROOM_ORDER.find(r=>r.toLowerCase()==='living room')||null;
/* the lowest a light dims to, as it says itself */
const floorOf=b=>{const c=capOf(b.target,'brightness');return c&&c.min!=null?c.min:10};
/* a light with no level of its own in a scene takes the scene's. Only a light that is reporting can say a scene is not on, and a light cannot go below its own floor */
function activeScene(){const known=D.bulbs.filter(b=>b.known);if(!known.length)return null;const mr=movieRoom();for(const [n,def] of Object.entries(sceneLevels())){const only=n==='Movie'&&mr;const ls=only?roomBulbs(mr).filter(b=>b.known):known;if(ls.length&&ls.every(b=>{const want=only?def.percent:(b.target in def.own?def.own[b.target]:def.percent);return want===0?!b.on:(b.on&&b.bri===Math.max(want,floorOf(b)))}))return n}return null}
/* room scenes: three made from the room's level, plus whatever the owner saved for it on the server */
function roomSceneDefs(room){const bs=roomBulbs(room);const d={Bright:bs.map(b=>[b.id,100]),Relax:bs.map(b=>[b.id,35]),Off:bs.map(b=>[b.id,0])};const r=roomByName(room);if(r)for(const s of r.scenes)d[s.name]=s.settings.map(x=>[BULBID[x.target],x.percent]).filter(([id])=>id&&bulbById(id));return d}
function activeRoomScene(room){if(!roomBulbs(room).some(b=>b.known))return null;for(const [n,def] of Object.entries(roomSceneDefs(room))){if(def.length&&def.every(([i,b])=>{const bl=bulbById(i);if(!bl||!bl.known)return true;return b===0?!bl.on:(bl.on&&bl.bri===b)}))return n}return null}
function runRoomScene(room,name,ctx){const a=name==='Bright'?ACT.roomLevel(room,100):name==='Relax'?ACT.roomLevel(room,35):name==='Off'?ACT.roomLevel(room,0):ACT.roomScene(room,name);a.label=room+' scene '+name;a.lean=Object.assign({},a.lean,{scene:name});dispatch(a,ctx)}
/* the capsule on Home: the picked room's scenes, or the whole house's */
function runCapsule(name,ctx){const r=pickedRoom();if(r&&name!=='Movie')runRoomScene(r,name,ctx);else runScene(name,ctx)}
function scenePending(name,room){const p=PENDING[room?'room-'+slug(room):'lights'];return !!(p&&p.lean&&p.lean.scene===name)}
function runScene(name,ctx){
  ctx=ctx||{};
  if(name==='Movie'){if(ctx.now){doNow(ACT.sceneMovie(),ctx);return}SHEET.stack=[];openSheet('lights',null,{replace:SHEET.open,action:ACT.sceneMovie(),channel:ctx.channel,transcript:ctx.transcript});return}
  const snap=D.bulbs.filter(b=>b.known).map(b=>({id:b.id,on:b.on,bri:b.bri}));const canUndo=snap.length===D.bulbs.length&&snap.length>0;
  const a=ACT.sceneApply(name,SCENE_TARGET[name]);if(ctx.now)doNow(a,ctx);else dispatch(a,ctx);if(ctx.tell)return;
  toast(name==='Lights off'?'Lights going off.':name+' is on its way.','',canUndo?[{l:'Undo',fn:()=>{snap.forEach(b=>run(b.on?ACT.bulbBri(b.id,b.bri):ACT.bulbOn(b.id,false)))}}]:[]);
}

export {movieRoom,activeScene,roomSceneDefs,activeRoomScene,runRoomScene,runCapsule,scenePending,runScene};
