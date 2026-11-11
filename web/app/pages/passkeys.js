import {$,$$,esc,ICON} from '../core/util.js';
import {ApiErr,api,passkeyCreate,passkeyWords} from '../core/api.js';
import {PASSKEYS,loadPasskeys} from '../core/store.js';
import {toast} from '../ui/toast.js';
import {A,ACT} from '../core/catalog.js';
import {armGuard} from '../core/guards.js';
import {SHEET,openSheet,fmtDay,fmtWhen} from '../ui/sheets.js';

const passkeysSheets={
  passkeys(){
    const html=`<div class="rows" id="pk-rows">${PASSKEYS.map(k=>`<div class="rowi" data-pkrow="${esc(k.id)}">${ICON('key')}<div class="t"><b>${esc(k.name)}${k.in_use?'<span class="tag">this device</span>':''}${k.recovery?'<span class="tag">recovery</span>':''}</b><small>added ${esc(fmtDay(k.created_at))} · last used ${k.last_used_at?esc(fmtWhen(k.last_used_at)):'never'}</small></div><button class="pill" data-revoke="${esc(k.id)}">Remove</button></div>`).join('')}</div>
      <div id="pk-form" hidden style="display:flex;flex-direction:column;gap:12px"><input class="field" id="pk-name" placeholder="A name for the new passkey" aria-label="Passkey name" maxlength="64" autocomplete="off"><div class="rowi">${ICON('usb')}<div class="t"><b>Recovery key</b><small>Keep it offline for when everything else is lost</small></div><button class="toggle" role="switch" aria-checked="false" aria-label="Recovery key" id="pk-rec"></button></div></div>
      <div class="acts" id="pk-acts"><button class="btn" id="pk-add">${ICON('plus')}Add a passkey</button><button class="btn" data-act="signout">${ICON('out')}Sign out here</button></div>
      <div id="pk-out"></div>
      <p class="p2">Passkeys are the only way in. There is no password.</p>`;
    const again=()=>{if(SHEET.type==='passkeys')openSheet('passkeys',null,{replace:true,focus:'keep'})};
    return {title:'Passkeys',html,mount(root){
      loadPasskeys().then(()=>{const ids=PASSKEYS.map(k=>k.id).join();if(ids!==$$('[data-pkrow]',root).map(x=>x.dataset.pkrow).join())again()}).catch(()=>{});
      $$('[data-revoke]',root).forEach(b=>b.onclick=()=>{const rowEl=b.closest('.rowi');const a=ACT.revoke(b.dataset.revoke);a.onDone=()=>{loadPasskeys().catch(()=>{}).then(()=>setTimeout(again,2700))};armGuard(a,rowEl,{channel:'touch'})});
      const rec=$('#pk-rec',root);rec.onclick=()=>rec.setAttribute('aria-checked',String(rec.getAttribute('aria-checked')!=='true'));
      $('#pk-add',root).onclick=()=>{const form=$('#pk-form',root);if(form.hidden){form.hidden=false;$('#pk-name',root).focus();return}const name=$('#pk-name',root).value.trim();if(!name){toast('Give the new passkey a name first.','');return}
        const a=A({action:'passkey.register',target:'new',params:{name,recovery:rec.getAttribute('aria-checked')==='true'},cls:'key',owner:true,key:'key-new',label:'Add the '+name+' passkey',holdLabel:'Hold to open the window',pkLabel:'First, approve with a passkey you already have'+(PASSKEYS.length===1?' ('+PASSKEYS[0].name+')':'')+'. Your new key comes after this.',pkWait:'Use your existing passkey now, not the new one.',targetName:name,deviceName:'passkey store',doneText:'Ready'});
        a.onDone=(rt,v)=>{const out=$('#pk-out',root);if(!v.registration||!out)return;out.innerHTML=`<div class="acts"><button class="btn primary" id="pk-create">${ICON('key')}Use this device</button><button class="btn" id="pk-fido">${ICON('usb')}Use a security key</button></div><p class="p2">A security key needs a PIN set on it.</p>`;const create=prefer=>async()=>{const bs=$$('#pk-create,#pk-fido',out);bs.forEach(x=>x.disabled=true);try{const credential=await passkeyCreate(v.registration.options,prefer);await api('POST','/passkeys/register/finish',{ceremony_id:v.registration.ceremony_id,credential});await loadPasskeys();toast('Passkey added.','ok');again()}catch(e){bs.forEach(x=>x.disabled=false);toast(e instanceof ApiErr?e.message:passkeyWords(e,prefer==='security-key'),'fail')}};$('#pk-create',out).onclick=create();$('#pk-fido',out).onclick=create('security-key')};
        armGuard(a,$('#pk-acts',root),{channel:'touch'})}}};
  },
};

export {passkeysSheets};
