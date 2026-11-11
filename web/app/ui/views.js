import {$,$$,ICON} from '../core/util.js';
import {ROOM_ORDER,S,isOwner,D} from '../core/store.js';
import {refractMaps} from './glass.js';
import {paintTile,roomTileEl,render} from '../pages/home.js';
import {roomOf} from './sheets.js';

/* ============================================================ views, routing, the scenes capsule as one liquid body */
function setView(v){S.view=v;$('#view-fav').hidden=v!=='fav';$('#view-rooms').hidden=v!=='rooms';if(v==='rooms'){renderRooms();if(location.hash!=='#rooms')history.replaceState(null,'','#rooms')}else if(location.hash==='#rooms')history.replaceState(null,'',location.pathname);scrollTo({top:0,behavior:'instant'});$('#top').classList.remove('compact');render()}
function renderRooms(){
  const grid=$('#rooms-grid');const key=ROOM_ORDER.join('|')+'|'+D.bulbs.map(b=>b.id+(roomOf('bulb'+b.id)||'')).join(',');
  if(grid.dataset.h!==key){grid.dataset.h=key;grid.innerHTML='';ROOM_ORDER.forEach(r=>grid.appendChild(roomTileEl(r,'rt')));refractMaps()}
  else $$('.tile',grid).forEach(t=>paintTile(t,'lights',t.dataset.room));
  const tools=$('#rooms-tools');const th=isOwner()?`<button class="btn" data-sheet="findlights">${ICON('plus')}Find new lights</button><button class="btn" data-sheet="arrange">${ICON('rooms')}Arrange rooms and lights</button>`:'';
  if(tools.dataset.h!==th){tools.innerHTML=th;tools.dataset.h=th}
  const hint=!ROOM_ORDER.length?(isOwner()?'No rooms yet. Add one under Arrange.':'No rooms yet.'):isOwner()?'Drag a room to set its lights. Tap it for scenes and names.':'Drag a room to set its lights. Tap it for scenes.';
  const he=$('#rooms-hint');if(he.textContent!==hint)he.textContent=hint;
}
function wireSceneLiquid(root){
  const lobes=$$('.ind i',root);
  $$('.seg',root).forEach((b,i)=>{
    const press=()=>{if(root.classList.contains('one')){lobes[0].classList.add('press');return}lobes[i].classList.add('press');if(lobes[i-1])lobes[i-1].classList.add('tug-r');if(lobes[i+1])lobes[i+1].classList.add('tug-l')};
    const release=()=>{lobes.forEach(l=>l.classList.remove('press','tug-l','tug-r'))};
    b.addEventListener('pointerdown',press);b.addEventListener('pointerup',release);b.addEventListener('pointercancel',release);b.addEventListener('pointerleave',release);
  });
}

export {setView,renderRooms,wireSceneLiquid};
