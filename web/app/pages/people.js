import {$,$$,esc,ICON} from '../core/util.js';
import {api,enc} from '../core/api.js';
import {USERS,INVITES,person,isOwner,loadUsers} from '../core/store.js';
import {toast} from '../ui/toast.js';
import {A} from '../core/catalog.js';
import {armGuard} from '../core/guards.js';
import {SHEET,openSheet,fmtDay,fmtWhen} from '../ui/sheets.js';
import {copyText} from '../console/rfb.js';

const peopleSheets={
  people(){
    const owner=isOwner();
    const html=`<div class="rows">${USERS.map(u=>`<div class="rowi">${ICON(u.role==='owner'?'key':'person')}<div class="t"><b>${esc(u.name)}${u.id===person().id?'<span class="tag">you</span>':''}</b><small>${u.role==='owner'?'owner':'family'} · ${u.passkeys} passkey${u.passkeys===1?'':'s'} · joined ${esc(fmtDay(u.created_at))}</small></div>${u.role==='family'?`<button class="pill" data-rmuser="${esc(u.id)}">Remove</button>`:''}</div>`).join('')||'<p class="quiet">Nobody yet.</p>'}</div>
      ${INVITES.length?`<h3 class="roomhead" style="font-size:17px">Waiting to join</h3><div class="rows">${INVITES.map(i=>`<div class="rowi">${ICON('clock')}<div class="t"><b>${esc(i.name)}</b><small>Good until ${esc(fmtWhen(i.expires_at))}</small></div><button class="pill" data-cancelinv="${esc(i.id)}">Cancel</button></div>`).join('')}</div>`:''}
      <h3 class="roomhead" style="font-size:17px">Invite someone</h3>
      <input class="field" id="pp-name" placeholder="A name to know them by" aria-label="Their name" maxlength="64" autocomplete="off">
      <div class="acts" id="pp-acts"><button class="btn primary" id="pp-invite">${ICON('plus')}Invite</button></div><div id="pp-out"></div>
      <p class="p2">An invitation works once, for a day. They get lights, music, the TV, the pets and the camera.</p>`;
    const again=()=>{if(SHEET.type==='people')openSheet('people',null,{replace:true,focus:'keep'})};
    return {title:'People',html,mount(root){if(!owner)return;
      loadUsers().then(()=>{if(USERS.length!==$$('[data-rmuser]',root).length+1||INVITES.length!==$$('[data-cancelinv]',root).length)again()}).catch(()=>{});
      $$('[data-rmuser]',root).forEach(b=>b.onclick=()=>{const u=USERS.find(x=>x.id===b.dataset.rmuser);const a=A({action:'user.remove',target:u.id,params:{},cls:'key',owner:true,key:'user-'+u.id,label:'Remove '+u.name,holdLabel:'Hold to remove',targetName:u.name,deviceName:'people list',doneText:'Removed'});a.onDone=()=>{loadUsers().catch(()=>{}).then(()=>setTimeout(again,2700))};armGuard(a,b.closest('.rowi'),{channel:'touch'})});
      $$('[data-cancelinv]',root).forEach(b=>b.onclick=async()=>{b.disabled=true;try{await api('DELETE','/invitations/'+enc(b.dataset.cancelinv));await loadUsers();again()}catch(e){b.disabled=false;toast(e.message,'fail')}});
      $('#pp-invite',root).onclick=()=>{const name=$('#pp-name',root).value.trim();if(!name){toast('Give them a name first.','');return}
        const a=A({action:'user.invite',target:'new',params:{name},cls:'key',owner:true,key:'user-new',label:'Invite '+name,holdLabel:'Hold to invite',targetName:name,deviceName:'people list',doneText:'Invitation made'});
        a.onDone=(rt,v)=>{const out=$('#pp-out',root);if(!out||!v.invitation)return;const link=location.origin+'/#invite='+v.invitation.token;out.innerHTML=`<p class="p2">Send this to ${esc(v.invitation.name)}. It is shown only now.</p><div class="copyrow"><code class="code" id="pp-link">${esc(link)}</code><button class="btn" id="pp-copy">Copy</button></div>`;$('#pp-copy',out).onclick=()=>copyText(link,$('#pp-copy',out),$('#pp-link',out));loadUsers().catch(()=>{})};
        armGuard(a,$('#pp-acts',root),{channel:'touch'})}}};
  },
};

export {peopleSheets};
