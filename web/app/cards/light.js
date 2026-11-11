import {$,$$,esc,ICON,PEND} from '../core/util.js';
import {api,enc} from '../core/api.js';
import {ROOM_ORDER,PLUGINS,DEVINFO,slug,S,isOwner,NR,D,stateOf,targetsOf,nameOf,refreshState,loadDevices} from '../core/store.js';
import {bulbName,roomBulbs,lightsSummary} from '../pages/home.js';
import {PENDING,scenePressed} from '../core/actions.js';
import {toast} from '../ui/toast.js';
import {HUES,ACT} from '../core/catalog.js';
import {runScene} from '../ui/scenes.js';
import {sliderEl,wireSlider} from '../ui/gestures.js';
import {dispatch,armGuard} from '../core/guards.js';
import {SHEET,openSheet,row,roomOf,failHtml} from '../ui/sheets.js';
import {FIND} from '../console/rfb.js';
import {setUpAgain} from '../pages/integrations.js';

const lightSheets={
  lights(arg,opts){opts=opts||{};
    const L=lightsSummary();
    const litHue=()=>{const lit=D.bulbs.filter(b=>b.on);return lit.length&&lit.every(b=>b.hue===lit[0].hue)?lit[0].hue:null};
    const rooms=ROOM_ORDER.filter(r=>roomBulbs(r).length);
    const loose=D.bulbs.filter(b=>!roomOf('bulb'+b.id));
    const roomRow=r=>{const bs=roomBulbs(r);const RL=lightsSummary(bs);return `<div class="bulbrow ${RL.n?'':'off'}" data-roomrow="${esc(r)}">${failHtml('room-'+slug(r))}
        <div class="bh"><span class="nm"><i style="--h:${RL.hue.toFixed(0)}"></i>${esc(r)}<small class="p2" style="font-size:15px" data-rst>${esc(RL.state)}</small><span class="pendslot">${PENDING['room-'+slug(r)]?PEND:''}</span></span><button class="pill" data-open="room:${esc(r)}" aria-label="${esc(r)} settings: each light, scenes and pin">Settings${ICON('chev')}</button></div>
        ${sliderEl('sl-r'+slug(r),RL.n?RL.avg:0,true,RL.hue.toFixed(0),r+' brightness',RL.state===NR?NR:RL.n?RL.avg+'%':'Off')}
      </div>`};
    const html=`${failHtml('lights')}
      <div class="segc" role="group" aria-label="Scenes">${['Movie','Evening','Focus','Lights off'].map(n=>`<button data-scene="${n}" aria-pressed="${scenePressed(null)===n}">${n}</button>`).join('')}</div>
      <div>${rooms.map(roomRow).join('')}</div>
      ${loose.length?`<p class="p2">${loose.length===1?'One light has no room yet: '+esc(bulbName(loose[0]))+'.':loose.length+' lights have no room yet.'}${isOwner()?' Place them under Rooms › Arrange.':''}</p>`:''}
      ${S.wall?'':`<div class="rows">${row('rooms','Rooms','Every room, with scenes and pins',null,'data-act="rooms"')}${isOwner()?row('plus','Find new lights','On the network, and behind a bridge',null,'data-open="findlights"'):''}</div>`}
      <details class="disc"><summary>Colour<svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></summary>
        <p class="p2">Sets every light that is on.</p>
        <div class="swatches" role="group" aria-label="Colour">${HUES.map(([n,h])=>`<button style="--h:${h}" aria-label="${n}" aria-pressed="${litHue()===h}" data-hue="${h}:${n}"></button>`).join('')}</div>
      </details>
      <p class="p2">Each light confirms its own change before it shows here.</p>`;
    return {title:'Lights',html,glowHue:L.hue,glowA:L.n?0.28:0,
      mount(root){
        $$('[data-scene]',root).forEach(b=>b.onclick=()=>{runScene(b.dataset.scene,{channel:'touch'})});
        $$('[data-hue]',root).forEach(s=>s.onclick=()=>{const [h,name]=s.dataset.hue.split(':');D.bulbs.filter(b=>b.on).forEach((b,i)=>setTimeout(()=>dispatch(ACT.bulbHue(b.id,+h,name),{channel:'touch'}),i*120))});
        rooms.forEach(r=>wireSlider($('#sl-r'+slug(r),root),{get:()=>{const RL=lightsSummary(roomBulbs(r));return RL.n?RL.avg:0},fmt:v=>v?v+'%':'Off',set:v=>dispatch(ACT.roomLevel(r,v),{channel:'touch'})}));
        if(opts.action)armGuard(opts.action,$('.segc',root),{channel:opts.channel,transcript:opts.transcript});},
      update(){const root=$('#sbody');$$('[data-scene]',root).forEach(b=>b.setAttribute('aria-pressed',String(scenePressed(null)===b.dataset.scene)));
        rooms.forEach(r=>{const el=$$('[data-roomrow]',root).find(x=>x.dataset.roomrow===r);if(!el)return;const bs=roomBulbs(r);const RL=lightsSummary(bs);el.classList.toggle('off',!RL.n);$('.nm i',el).style.setProperty('--h',RL.hue.toFixed(0));$('[data-rst]',el).textContent=RL.state;$('.pendslot',el).innerHTML=PENDING['room-'+slug(r)]||bs.some(b=>PENDING['bulb'+b.id])?PEND:'';
          const sl=$('#sl-r'+slug(r),el);if(sl&&!sl.classList.contains('dragging')){const v=RL.n?RL.avg:0;sl.style.setProperty('--p',v.toFixed(1));sl.style.setProperty('--gh',RL.hue.toFixed(0));sl.setAttribute('aria-valuenow',Math.round(v));$$('.num',sl).forEach(n=>n.textContent=RL.state===NR?NR:v?Math.round(v)+'%':'Off')}});
        const lh=litHue();$$('[data-hue]',root).forEach(s=>s.setAttribute('aria-pressed',String(+s.dataset.hue.split(':')[0]===lh)));
        const ok=lightsSummary();SHEET.el.style.setProperty('--gh',ok.hue);SHEET.el.style.setProperty('--ga',ok.n?0.28:0)}};
  },
  /* looking for lights asks every integration that has lights; a bridge is paired by setting its integration up again */
  findlights(){
    const F=FIND;const bridges=targetsOf('bridge');
    const pluginName=id=>{const p=PLUGINS.plugins.find(x=>x.id===id);return p?(p.name||p.id):String(id||'')};
    const at=a=>/^[0-9a-f:.]+$/i.test(String(a||''))&&/[.:]/.test(String(a||''));
    const list=F.found.map(f=>`<div class="rowi">${ICON('bulb')}<div class="t"><b>${esc(pluginName(f.kind))} <span class="fine">${esc(f.name||f.mac)}</span></b><small>${f.added?(f.moved?'Known, but it has moved to '+esc(f.address):'Already added'):at(f.address)?'New, at '+esc(f.address):'New, behind a bridge'}</small></div>${f.added?(f.moved&&f.target?`<button class="pill on" data-fl-move="${esc(f.target)}">Accept</button>`:''):`<button class="pill on" data-fl-add="${esc(f.mac)}">Add</button>`}</div>`).join('');
    const bridgeRow=t=>`<div class="rowi">${ICON('tech')}<div class="t"><b>${esc(nameOf(t))}</b><small>${esc(F.pairMsg||(stateOf(t)?'Paired. Its lights can be added below.':'Press the pairing button on the bridge once, then pair.'))}</small></div>${F.pair==='asking'?PEND:stateOf(t)?'':`<button class="pill on" data-fl-pair="${esc(t)}">Pair</button>`}</div>`;
    const html=`<p class="p2">Looks for lights on the network, and for the lights behind ${bridges.length===1?'the bridge':'each bridge'}.</p>
      <div class="rows">
        <div class="rowi">${ICON('bulb')}<div class="t"><b>On the network</b><small>${esc(F.lookMsg||'Not looked yet')}</small></div>${F.look==='looking'?PEND:''}</div>
        ${bridges.map(bridgeRow).join('')}
      </div>
      ${list?`<div class="rows">${list}</div>`:''}
      <div class="acts"><button class="btn primary" id="fl-start" ${F.look==='looking'?'disabled':''}>${F.look==='idle'?'Start looking':'Look again'}</button></div>
      <p class="p2">A new light starts off and has no room. Name it and place it from its room.</p>`;
    const re=()=>{if(SHEET.type==='findlights')openSheet('findlights',null,{replace:true,focus:'keep'})};
    return {title:'Find new lights',html,mount(root){
      $('#fl-start',root).onclick=async()=>{F.look='looking';F.lookMsg='Looking…';F.found=[];re();try{const r=await api('POST','/lights/discover',{});F.found=r.found||[];if(!r.simulated&&F.found.length){const known=await api('GET','/lights');for(const f of F.found){const l=(known.lights||[]).find(x=>x.mac===f.mac);if(l)f.target=l.target}}const behind=F.found.filter(f=>!at(f.address)).length,own=F.found.length-behind;F.lookMsg=r.simulated?'Dry-run: nothing was sent, so nothing was found.':own?'Found '+own+'.':'Nothing answered.';if(r.hue_error)F.pairMsg='The bridge did not list its lights: '+r.hue_error;else if(behind)F.pairMsg='Found '+behind+' behind the bridge. Add them below.'}catch(e){F.lookMsg=e.message}F.look='done';re()};
      /* pairing a bridge is its integration's own setup: the address, the button, then which lights. A bridge that has no integration behind it pairs the old way, through the light list's own route */
      $$('[data-fl-pair]',root).forEach(pr=>pr.onclick=async()=>{const t=pr.dataset.flPair;const inst=(DEVINFO[t]||{}).instance;if(inst){setUpAgain(inst,pr);return}
        F.pair='asking';F.pairMsg='Asking the bridge…';re();try{const r=await api('POST','/lights/hue/pair',{});await refreshState();F.pairMsg=r.simulated?'Dry-run: nothing was sent to the bridge.':'Paired. Look for lights to add them below.'}catch(e){F.pairMsg=e.message}F.pair='idle';re()});
      $$('[data-fl-add]',root).forEach(b=>b.onclick=async()=>{b.disabled=true;const mac=b.dataset.flAdd;const f=F.found.find(x=>x.mac===mac);try{const r=await api('POST','/lights',{mac,name:(f&&f.name)||'New light '+mac.slice(-4)});if(f)f.added=true;await loadDevices();await refreshState();const nb=D.bulbs.find(x=>x.target===r.light.target);if(nb)openSheet('where:bulb'+nb.id,null,{replace:true});else re()}catch(e){b.disabled=false;toast(e.message,'fail')}});
      $$('[data-fl-move]',root).forEach(b=>b.onclick=async()=>{b.disabled=true;try{await api('PUT','/lights/'+enc(b.dataset.flMove)+'/address',{});const f=F.found.find(x=>x.target===b.dataset.flMove);if(f)f.moved=false;re()}catch(e){b.disabled=false;toast(e.message,'fail')}})}};
  },
};

export {lightSheets};
