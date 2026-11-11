import {$,$$} from './core/util.js';
import './core/api.js';
import {S,D,crashCount,reporting,tick} from './core/store.js';
import './core/session.js';
import './core/status.js';
import {queueHighlights} from './ui/roomlight.js';
import {raysKick,refractMaps} from './ui/glass.js';
import {dupWidths,pickedRoom,wireRoomPick,compactTop,render,homeSheets} from './pages/home.js';
import './core/actions.js';
import './ui/toast.js';
import {ACT} from './core/catalog.js';
import {runCapsule} from './ui/scenes.js';
import {wireTile} from './ui/gestures.js';
import {dispatch} from './core/guards.js';
import {SHEET,openSheet,closeSheet} from './ui/sheets.js';
import {monitorSheets} from './pages/monitor.js';
import {roomsSheets} from './pages/rooms.js';
import {lightSheets} from './cards/light.js';
import {speakerSheets} from './cards/speaker.js';
import {vmSheets} from './cards/vm.js';
import {feederSheets} from './cards/feeder.js';
import {cameraSheets} from './cards/camera.js';
import {printerSheets} from './cards/printer.js';
import {auditSheets} from './pages/audit.js';
import {passkeysSheets} from './pages/passkeys.js';
import {memorySheets} from './pages/memory.js';
import {peopleSheets} from './pages/people.js';
import {plugSheets} from './cards/plug.js';
import {airconSheets} from './cards/aircon.js';
import {cardsSheets} from './cards/index.js';
import {genericSheets} from './cards/generic.js';
import {hostSheets} from './cards/host.js';
import {routerSheets} from './cards/router.js';
import {serviceSheets} from './cards/service.js';
import {telemetrySheets} from './cards/telemetry.js';
import './cards/tv.js';
import './cards/litter.js';
import {integrationsSheets} from './pages/integrations.js';
import {setupSheets} from './pages/setup.js';
import './core/events.js';
import './console/rfb.js';
import './console/des.js';
import './swifty/parser.js';
import {VOICE,startListening,stopListening,wireCapsule,voiceSheets} from './swifty/voice.js';
import {chatSheets} from './swifty/chat.js';
import './swifty/figure.js';
import {lock,boot} from './ui/lock.js';
import {WALL,exitWall} from './ui/dock.js';
import {setView,wireSceneLiquid} from './ui/views.js';
import {route} from './core/router.js';

/* ============================================================ sheet contents */
const SHEETS={...lightSheets,...speakerSheets,...vmSheets,...feederSheets,...cameraSheets,...printerSheets,...homeSheets,...auditSheets,...passkeysSheets,...memorySheets,...peopleSheets,...monitorSheets,...roomsSheets,...plugSheets,...airconSheets,...cardsSheets,...genericSheets,...hostSheets,...routerSheets,...serviceSheets,...telemetrySheets,...integrationsSheets,...setupSheets,...chatSheets,...voiceSheets};

/* ============================================================ wiring */
wireTile($('#t-lights'),'lights',pickedRoom);wireTile($('#t-music'),'music');wireRoomPick($('#roompick'));
$('#tvbar').addEventListener('click',e=>{const b=e.target.closest('[data-tvk],[data-tvbar]');if(!b||b.disabled)return;if(b.dataset.tvk)dispatch(ACT.tvKey(b.dataset.tvk,b.getAttribute('aria-label')),{channel:'touch'});else if(b.dataset.tvbar==='power')openSheet('music',b,{action:ACT.tvPower(!D.tv.on),channel:'touch'});else openSheet('music',b)});
$('#acs').addEventListener('click',e=>{const b=e.target.closest('[data-ac]');if(!b||b.disabled)return;const c=D.acs.find(x=>x.target===b.dataset.ac);if(c)dispatch(ACT.acPower(c.target,!c.on),{channel:'touch'})});
$('#plugs').addEventListener('click',e=>{const b=e.target.closest('[data-plug]');if(!b||b.disabled)return;const p=D.plugs.find(x=>x.target===b.dataset.plug);if(p)dispatch(ACT.plug(p.target,!p.on),{channel:'touch'})});
$$('#scenes .seg').forEach(b=>b.onclick=()=>runCapsule(b.dataset.scene,{channel:'touch'}));wireSceneLiquid($('#scenes'));
/* a tile whose machine logged a crash opens its page at Health, with the counts in view */
document.addEventListener('click',e=>{if(S.locked)return;const s=e.target.closest('[data-sheet]');if(s&&!s.closest('#sheet')){const t=s.dataset.sheet;const key=t==='host'?'host':null;openSheet(t,s,key&&reporting(key)&&crashCount(key)>0?{jump:'health'}:undefined)}});
wireCapsule($('#cap-top'),{});wireCapsule($('#cap-bottom'),{});
$('#rooms-back').onclick=()=>setView('fav');
addEventListener('hashchange',route);
/* the status sentence stays on screen: once the page scrolls, the header tightens */
addEventListener('scroll',compactTop,{passive:true});
addEventListener('keydown',e=>{
  if(e.key==='Escape'){if(SHEET.open)closeSheet();else if(S.wall)exitWall();return}
  if(e.target&&e.target.matches&&e.target.matches('input,textarea'))return;
  if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='k'){e.preventDefault();if(!S.locked)openSheet('more',null,{focus:'field'});return}
  if(e.key==='/'&&!S.locked){e.preventDefault();openSheet('more',null,{focus:'field'});return}
  if(e.key.toLowerCase()==='v'&&!e.repeat&&!S.locked){startListening()}
});
addEventListener('keyup',e=>{if(e.key.toLowerCase()==='v'&&VOICE.on)stopListening()});
/* iPadOS answers a long press by selecting nearby text, even beside controls that opt out, so nothing can be selected while a finger holds one */
{const doc=document.documentElement,held=e=>e.target.closest&&e.target.closest('.press,.capsule,.ptt,.hold,.slider,.tile,button');
addEventListener('pointerdown',e=>{if(e.pointerType==='mouse'||!held(e))return;doc.classList.add('holding');try{getSelection().removeAllRanges()}catch(err){}},{passive:true,capture:true});
['pointerup','pointercancel'].forEach(n=>addEventListener(n,()=>doc.classList.remove('holding'),{passive:true,capture:true}));
document.addEventListener('selectstart',e=>{if(doc.classList.contains('holding'))e.preventDefault()});}
addEventListener('pointerdown',()=>{WALL.lastTouch=Date.now()},{passive:true,capture:true});
let idleLockT=0;const armIdleLock=()=>{clearTimeout(idleLockT);idleLockT=setTimeout(()=>{if(!S.wall&&!S.locked)lock('Locked after a while away.')},15*60e3)};
addEventListener('pointerdown',armIdleLock,{passive:true});addEventListener('keydown',armIdleLock);armIdleLock();
new ResizeObserver(()=>{dupWidths();queueHighlights();refractMaps()}).observe(document.body);
render();setInterval(tick,1000);boot();
route();refractMaps();setTimeout(refractMaps,300);raysKick();
document.fonts&&document.fonts.ready.then(()=>{dupWidths();queueHighlights()});

export {SHEETS};
