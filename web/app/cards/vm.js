import {$,$$,esc,ICON,PEND} from '../core/util.js';
import {api} from '../core/api.js';
import {VMS,HOSTS,T,TGT,vmById,isOwner,NR,refreshSoon,loadMachines,reporting} from '../core/store.js';
import {COMP_SINCE,COMP_SUB} from '../pages/home.js';
import {PENDING,run} from '../core/actions.js';
import {toast} from '../ui/toast.js';
import {A,ACT} from '../core/catalog.js';
import {pkStep,armGuard} from '../core/guards.js';
import {SHEET,openSheet,row,failHtml,fmtDay,fmtWhen} from '../ui/sheets.js';
import {chartGet,lastOf,chartHtml,wireChart,paintCharts,jumpTo,rangeHtml,wireRange,healthHtml} from '../pages/monitor.js';
import {placeRow} from '../pages/rooms.js';
import {CONSOLE,ADDM,copyText,openConsole} from '../console/rfb.js';
import {notesBlock,wireNotes} from './index.js';

const vmSheets={
  comp(id,opts){
    const v=vmById(id);if(!v)return {title:'Computer',html:'<p class="quiet">That computer is not on the list any more.</p>'};
    const on=v.known&&v.state==='running';const rep=reporting(v.id);const off=v.known&&v.state!=='running';
    const verbs=mode=>mode==='on'?`<button class="btn" data-verb="restart">Restart</button><button class="btn danger" data-verb="stop">Stop</button>`:mode==='off'?`<button class="btn primary" data-verb="start">Start</button>`:mode==='busy'?'':`<button class="btn primary" data-verb="start">Start</button><button class="btn" data-verb="restart">Restart</button><button class="btn danger" data-verb="stop">Stop</button>`;
    const modeNow=()=>v.transition?'busy':!v.known?'unknown':v.state==='running'?'on':'off';
    /* a console only where the host offers one for this computer */
    const consoleBlock=()=>{if(!isOwner())return '';
      if(!v.console)return `<h3 class="roomhead" style="font-size:17px">Console</h3><p class="p2">This computer has no console here. Its screen only shows where it is plugged in.</p>`;
      return `<h3 class="roomhead" style="font-size:17px" id="con-head">Console</h3><div id="con-area" hidden></div><div class="acts" id="con-acts"><button class="btn" id="con-open" ${off?'disabled':''}>${ICON('term')}Open console</button></div><p class="p2">Each session needs a fresh passkey touch and is written to History.</p>`};
    const html=`${failHtml(v.id)}
      <p class="p2"><span class="fine">${esc(v.tech)}</span></p>
      ${v.pending?`<div class="confirm"><p class="sent">Waiting for your approval.</p><p class="p2">It appeared by itself. Approve it with your passkey before it can start.</p><div class="acts"><button class="btn primary" data-open="addmachine">Approve under Add machine</button></div></div>`:`<div class="rowi">${ICON(v.icon)}<div class="t"><b id="comp-since">${esc(COMP_SINCE(v))}</b><small id="comp-sub">${esc(COMP_SUB(v))}</small></div><span id="comp-pend">${PENDING[v.id]||v.transition?PEND:''}</span></div>
      <div class="acts" id="comp-acts" data-mode="${modeNow()}">${isOwner()?verbs(modeNow()):''}</div>`}
      ${v.pending?'':`${rangeHtml()}<div class="rows">${row(null,'Load','How busy its processors are','<span id="comp-load"></span>')}</div>${chartHtml('comp-chart','Load over time')}`}
      ${consoleBlock()}
      ${notesBlock(v.id)}
      ${healthHtml(v.id)}
      ${placeRow(v.id)}
      <details class="disc"><summary>Details<svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></summary><dl class="facts"><dt>Listed as</dt><dd>${esc(v.tech)}</dd><dt>First seen</dt><dd>${v.firstSeen?esc(fmtDay(v.firstSeen)):'From the start'}</dd><dt>Approved</dt><dd>${v.pending?'Not yet':v.approvedAt?esc(fmtDay(v.approvedAt)):'From the start'}</dd></dl></details>`;
    return {title:v.name,html,glowHue:270,glowA:0.1,
      mount(root){const acts=$('#comp-acts',root);root.dataset.rep=String(rep);
        const wire=()=>$$('[data-verb]',root).forEach(b=>b.onclick=()=>armGuard(ACT.vm(b.dataset.verb,v.id),acts,{channel:'touch'}));wire();
        if(opts.action&&acts){armGuard(opts.action,acts,{channel:opts.channel,transcript:opts.transcript})}
        wireRange(root);const ch=$('#comp-chart',root);if(ch)wireChart(ch,()=>chartGet('vm:'+v.id,{min:0,max:100,fmt:x=>Math.round(x)+'%'}));
        const co=$('#con-open',root);if(co)co.onclick=()=>openConsole(v,root);wireNotes(root);
        jumpTo(root,opts.jump==='health'?'#hl-'+v.id:opts.jump==='console'?'#con-head':null);},
      update(){const root=$('#sbody');const s=$('#comp-since',root);if(s)s.textContent=COMP_SINCE(v);const sb=$('#comp-sub',root);if(sb)sb.textContent=COMP_SUB(v);const p=$('#comp-pend',root);if(p)p.innerHTML=PENDING[v.id]||v.transition?PEND:'';
        const ld=$('#comp-load',root);if(ld){const n=lastOf('vm:'+v.id);ld.textContent=n==null?NR:Math.round(n)+'%'}
        paintCharts(root);
        const acts=$('#comp-acts',root);if(acts&&isOwner()&&!$('[data-guard]',root)&&!acts.hidden){const want=modeNow();if(acts.dataset.mode!==want){acts.dataset.mode=want;acts.innerHTML=verbs(want);$$('[data-verb]',acts).forEach(b=>b.onclick=()=>armGuard(ACT.vm(b.dataset.verb,v.id),acts,{channel:'touch'}))}}
        const co=$('#con-open',root);if(co&&!CONSOLE.open)co.disabled=v.known&&v.state!=='running'}};
  },
  addmachine(){
    /* only what this home can do: approving and further hosts need a host, pairing an agent needs a collector */
    if((ADDM.tab==='agent'&&!TGT.telemetry)||(ADDM.tab!=='agent'&&!TGT.host))ADDM.tab=TGT.host?'vm':'agent';
    const tab=ADDM.tab;const pending=VMS.filter(v=>v.pending);
    const panels={
      vm:`<p class="p2">A new computer made on ${esc(T.hostName)} shows up here by itself. It needs your passkey before it can be started or restarted.</p>
        ${pending.length?pending.map(v=>`<div class="rowi">${ICON(v.icon)}<div class="t"><b>${esc(v.name)} <span class="tag">${esc(v.tech)}</span></b><small>First seen ${v.firstSeen?esc(fmtWhen(v.firstSeen)):'earlier'}. Waiting for your approval.</small></div></div><div data-am-pk="${esc(v.id)}" hidden></div><div class="acts" data-am-appr="${esc(v.id)}"><button class="btn primary" data-approve="${esc(v.id)}">Approve with passkey</button></div>`).join(''):`<p class="quiet">Nothing is waiting.</p>`}
        <div class="acts"><button class="btn" id="am-scan">Look for new computers</button></div><p class="p2" id="am-scan-out"></p>`,
      host:`<p class="p2">Another host of the same kind as ${esc(T.hostName)}. Swifty Home reads its state with a token kept in a file on this server. The token never passes through this page.</p>
        <input class="field" id="am-id" placeholder="Short name, like host2" aria-label="Short name" maxlength="16" autocomplete="off">
        <input class="field" id="am-addr" placeholder="Address, like 192.0.2.20:8006" aria-label="Address" autocomplete="off">
        <input class="field" id="am-node" placeholder="Node name, as the host calls itself" aria-label="Node name" autocomplete="off">
        <input class="field" id="am-token" placeholder="Token file name, on the server" aria-label="Token file name" autocomplete="off">
        <input class="field" id="am-ca" placeholder="CA file name, on the server" aria-label="CA file name" autocomplete="off">
        <div class="acts" id="am-host-acts"><button class="btn primary" id="am-addhost">Add host</button></div>
        ${HOSTS.length?`<h3 class="roomhead" style="font-size:17px">Further hosts</h3><div class="rows">${HOSTS.map(h=>`<div class="rowi">${ICON('tech')}<div class="t"><b>${esc(h.id)}</b><small>${esc(h.address)} · ${esc(h.node)}</small></div><button class="pill" data-rmhost="${esc(h.id)}">Remove</button></div>`).join('')}</div>`:''}`,
      agent:`<p class="p2">For any other computer, pair a reporting agent with the collector. Agents only report: nothing here can start, stop or change that computer.</p>
        <input class="field" id="am-agent" placeholder="A name for that computer" aria-label="Computer name" maxlength="64" autocomplete="off">
        <div class="acts" id="am-agent-acts"><button class="btn primary" id="am-pair">Start pairing</button></div><div id="am-pair-out"></div>`};
    const html=`<div class="segc" role="group" aria-label="How to add it">${[['vm','On '+T.hostName],['host','Another host'],['agent','Any computer']].filter(([k])=>k==='agent'?!!TGT.telemetry:!!TGT.host).map(([k,l])=>`<button data-am="${k}" aria-pressed="${tab===k}">${l}</button>`).join('')}</div>
      <div id="am-panel">${panels[tab]}</div>`;
    const re=()=>{if(SHEET.type==='addmachine')openSheet('addmachine',null,{replace:true,focus:'keep'})};
    const flag=(id,params,label,extra)=>A(Object.assign({action:id,target:'new',params,cls:'key',owner:true,key:'am-'+id,label,holdLabel:'Hold to continue',targetName:label,deviceName:'machine list',doneText:'Done'},extra||{}));
    return {title:'Add machine',html,mount(root){
      $$('[data-am]',root).forEach(b=>b.onclick=()=>{ADDM.tab=b.dataset.am;re()});
      $$('[data-approve]',root).forEach(b=>b.onclick=()=>{const id=b.dataset.approve;const host=$(`[data-am-pk="${id}"]`,root);host.hidden=false;$(`[data-am-appr="${id}"]`,root).hidden=true;const a=ACT.approve(id);a.channel='touch';pkStep(host,'Touch to approve this computer with your passkey',a,()=>{a.onDone=()=>{loadMachines().catch(()=>{}).then(()=>{refreshSoon();setTimeout(re,450)})};run(a)})});
      const sc=$('#am-scan',root);if(sc)sc.onclick=async()=>{sc.disabled=true;const o=$('#am-scan-out',root);try{const r=await api('POST','/machines/scan',{});if(r.simulated)o.textContent='Dry-run: nothing was asked, so nothing new was found.';else{await loadMachines();o.textContent=r.added&&r.added.length?r.added.length+' new.':'Nothing new.';setTimeout(re,900)}}catch(e){o.textContent=e.message}sc.disabled=false};
      const ah=$('#am-addhost',root);if(ah)ah.onclick=()=>{const v=id=>$(id,root).value.trim();const params={id:v('#am-id'),address:v('#am-addr'),node:v('#am-node'),token_file:v('#am-token'),ca_file:v('#am-ca')};if(Object.values(params).some(x=>!x)){toast('Fill in every field first.','');return}const a=flag('proxmox.add_host',params,'Add host '+params.id);a.onDone=()=>{loadMachines().catch(()=>{}).then(()=>setTimeout(re,2700))};armGuard(a,$('#am-host-acts',root),{channel:'touch'})};
      $$('[data-rmhost]',root).forEach(b=>b.onclick=()=>{const a=flag('proxmox.remove_host',{},'Remove host '+b.dataset.rmhost);a.target=b.dataset.rmhost;a.onDone=()=>{loadMachines().catch(()=>{}).then(()=>setTimeout(re,2700))};armGuard(a,b.closest('.rowi'),{channel:'touch'})});
      const pr=$('#am-pair',root);if(pr)pr.onclick=()=>{const name=$('#am-agent',root).value.trim();if(!name){toast('Give that computer a name first.','');return}const a=flag('agent.pair',{name},'Pair '+name);a.onDone=(rt,v)=>{const out=$('#am-pair-out',root);if(!out)return;if(!v.pairing){out.innerHTML='<p class="p2">Dry-run: nothing was asked of the collector, so there is no pairing to show.</p>';return}const text=Object.entries(v.pairing).map(([k,x])=>k+': '+x).join('\n');out.innerHTML=`<p class="p2">Shown only now.</p><div class="copyrow"><code class="code" id="am-cmd">${esc(text)}</code><button class="btn" id="am-copy">Copy</button></div>`;$('#am-copy',out).onclick=()=>copyText(text,$('#am-copy',out),$('#am-cmd',out))};armGuard(a,$('#am-agent-acts',root),{channel:'touch'})}}};
  },
};

export {vmSheets};
