import {$,$$,rnd,hhmm,reduced} from '../core/util.js';
import {S,D} from '../core/store.js';
import {problems,status} from '../core/status.js';
import {refractMaps} from './glass.js';
import {paintTile,pickedRoom,paintRoomPick,wireRoomPick,petWord,paintScenes,render} from '../pages/home.js';
import {runCapsule} from './scenes.js';
import {wireTile} from './gestures.js';
import {SHEET,springEasing,openSheet,closeSheet} from './sheets.js';
import {VOICE,wireCapsule} from '../swifty/voice.js';
import {wireSceneLiquid} from './views.js';

/* ============================================================ wall dock */
const WALL={idle:null,lastTouch:Date.now(),wired:false,lock:null,driftT:null,night:false,prevTheme:null};
function touched(){WALL.lastTouch=Date.now();if(S.wall){const w=$('#wall');if(w.classList.contains('idle')){w.classList.remove('idle');if(!reduced)w.animate([{opacity:.6,transform:'scale(.985)'},{opacity:1,transform:'none'}],{duration:500,easing:springEasing()})}}}
function enterWall(){
  if(S.wall)return;S.wall=true;closeSheet();if(location.hash!=='#wall')history.replaceState(null,'','#wall');
  const w=$('#wall');w.hidden=false;$('#app').hidden=true;$('#cap-bottom').hidden=true;document.documentElement.classList.add('wall');
  if(!WALL.wired){WALL.wired=true;wireTile($('#w-lights'),'lights',pickedRoom);wireTile($('#w-music'),'music');wireRoomPick($('#wall-roompick'));wireCapsule($('#w-ptt'),{holdOnly:true});
    $$('#wall-scenes .seg').forEach(b=>b.onclick=()=>runCapsule(b.dataset.scene,{channel:'touch'}));wireSceneLiquid($('#wall-scenes'));
    $('#w-pets').onclick=()=>openSheet('pets',$('#w-pets'));$('#wall-exit').onclick=()=>exitWall();
    w.addEventListener('pointerdown',touched,{capture:true});w.addEventListener('pointerdown',e=>{if(w.classList.contains('attention')&&e.target.closest('.attn')){w.classList.remove('attention');WALL.attnSeen=problems()[0]?.key}},{capture:true});}
  WALL.lastTouch=Date.now();WALL.attnSeen=null;
  try{if(navigator.wakeLock&&navigator.wakeLock.request)navigator.wakeLock.request('screen').then(l=>{WALL.lock=l}).catch(()=>{})}catch(e){}
  renderWall();refractMaps();
}
function exitWall(silent){
  if(!S.wall)return;S.wall=false;const w=$('#wall');w.hidden=true;w.classList.remove('idle','night','attention');$('#app').hidden=false;$('#cap-bottom').hidden=false;document.documentElement.classList.remove('wall','night');
  if(WALL.night){WALL.night=false;const root=document.documentElement;if(WALL.prevTheme)root.setAttribute('data-theme',WALL.prevTheme);else root.removeAttribute('data-theme');WALL.prevTheme=null}
  if(location.hash==='#wall'&&!silent)history.replaceState(null,'',location.pathname);
  try{WALL.lock&&WALL.lock.release&&WALL.lock.release()}catch(e){}WALL.lock=null;render();
}
function renderWall(){
  const w=$('#wall');const st=status();const now=new Date();
  $('#wall-title').textContent=st.text;$('#wall-clock').textContent=hhmm(now);$('#bigclock').textContent=hhmm(now);$('#ambient-sentence').textContent=st.text;
  $('#w-pets').hidden=!D.feeder.target&&!D.litter.target;$('#w-pets-st').textContent=petWord();
  paintRoomPick($('#wall-roompick'));paintScenes($('#wall-scenes'));paintTile($('#w-lights'),'lights',pickedRoom());paintTile($('#w-music'),'music');
  /* night on the wall: the room darkens and the dark palette takes over, so every word keeps its contrast */
  const h=now.getHours();const night=h>=22||h<6;w.classList.toggle('night',night);
  const root=document.documentElement;if(night!==WALL.night){WALL.night=night;root.classList.toggle('night',night);if(night){WALL.prevTheme=root.getAttribute('data-theme');root.setAttribute('data-theme','dark')}else if(WALL.prevTheme!=null){if(WALL.prevTheme)root.setAttribute('data-theme',WALL.prevTheme);else root.removeAttribute('data-theme');WALL.prevTheme=null}}
  const p=problems()[0];
  if(p&&WALL.attnSeen!==p.key){
    if(!w.classList.contains('attention')){w.classList.add('attention');w.classList.remove('idle');WALL.lastTouch=Date.now()}
    $('#attn-text').textContent=p.text;
  }else w.classList.remove('attention');
  if(!p)WALL.attnSeen=null;
  const idle=Date.now()-WALL.lastTouch>60000&&!SHEET.open&&!VOICE.on&&!w.classList.contains('attention');
  if(idle&&!w.classList.contains('idle')){w.classList.add('idle');drift()}
}
function drift(){const a=$('#ambient');if(!a||reduced)return;a.style.transform=`translate(${Math.round(rnd(-16,16))}px,${Math.round(rnd(-14,14))}px)`}
setInterval(()=>{if(S.wall&&$('#wall').classList.contains('idle'))drift()},60000);

export {WALL,touched,enterWall,exitWall,renderWall};
