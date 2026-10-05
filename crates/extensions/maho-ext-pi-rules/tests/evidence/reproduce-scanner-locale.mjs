const pin='12ad906f0b29e949ebbd1f89d8f85789578aa6e6';
const response=await fetch(`https://raw.githubusercontent.com/code-yeongyu/pi-rules/${pin}/src/rules/scanner.ts`);
if(!response.ok)throw new Error(`Source ${response.status}`);
const text=await response.text();
if(!text.includes('leftEntry.name.localeCompare(rightEntry.name)'))throw new Error('Source sort contract changed');
const binary=process.env.CARGO_TARGET_DIR+'/debug/examples/scanner_qa';
const cases=[{LANG:'en_US.UTF-8',LC_ALL:'en_US.UTF-8'},{LANG:'sv_SE.UTF-8',LC_ALL:'sv_SE.UTF-8'},{LANG:'en_US.UTF-8',LC_MESSAGES:'sv_SE.UTF-8'},{LANG:'sv_SE.UTF-8',LC_ALL:'C'},{LC_ALL:'',LC_MESSAGES:'',LANG:'sv_SE.UTF-8'},{LANG:'sv_SE.UTF-8',LC_ALL:''},{LANG:'sv_SE.UTF-8',LC_ALL:'invalid'},{LANG:'not_a_locale'},{LANG:'sv_SE.UTF-8@euro'}];
for(const locale of cases){
    const env={PATH:'/usr/bin:/bin',...locale};
    const oracle=Bun.spawn(['node','-e','console.log(JSON.stringify(["z.md","ä.md","å.md","a.md","ö.md"].sort((a,b)=>a.localeCompare(b))))'],{env,stdout:'pipe',stderr:'inherit'});
    const native=Bun.spawn([binary],{env,stdout:'pipe',stderr:'inherit'});
    const expected=await new Response(oracle.stdout).text();const actual=await new Response(native.stdout).text();
    const exits=[await oracle.exited,await native.exited];
    if(exits.some(x=>x!==0)||expected.trim()!==actual.trim())throw new Error(JSON.stringify({locale,expected,actual,exits}));
    console.log(JSON.stringify({pin,source_sha256:new Bun.CryptoHasher('sha256').update(text).digest('hex'),locale,names:JSON.parse(actual),exits}));
}
