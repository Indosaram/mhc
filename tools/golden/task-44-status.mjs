import { resolve,join } from "node:path";
import { execFileSync } from "node:child_process";
import { mkdirSync,writeFileSync } from "node:fs";
import { pinnedSenpiRoot } from "./pin.mjs";
const senpi=pinnedSenpiRoot();
const upstream=resolve(process.env.OMO_SRC ?? "/home/indo/code/oh-my-openagent");
const expected="77f3067f157a4f88e6d8ed48b3a6c338654402ed";
const head=execFileSync("git",["--no-pager","-C",upstream,"rev-parse","HEAD"],{encoding:"utf8"}).trim();
if(head!==expected) throw new Error(`omo source pin mismatch: ${head}`);
const result=await Bun.build({entrypoints:[join(upstream,"packages/omo-senpi/src/components/task/status-row-format.ts")],target:"bun",plugins:[{name:"pinned-status",setup(builder){
    builder.onResolve({filter:/^@earendil-works\/pi-tui$/},()=>({path:join(senpi,"packages/tui/src/index.ts")}));
    builder.onResolve({filter:/^@oh-my-opencode\/senpi-task$/},()=>({path:"status-helpers",namespace:"status-source"}));
    builder.onLoad({filter:/.*/,namespace:"status-source"},()=>({loader:"ts",contents:`export { excerptRendererText,normalizeRendererText,rendererVisibleWidth } from ${JSON.stringify(join(upstream,"packages/senpi-task/src/renderer-text.ts"))};\nexport { formatLiveSpend,formatStatusTarget,formatTargetWithModel,taskIdentityLabel,toolCountSuffix } from ${JSON.stringify(join(upstream,"packages/senpi-task/src/status-line.ts"))};`}));
}}]});
if(!result.success) throw new AggregateError(result.logs,"Status bundle failed");
const status=await import(`data:text/javascript;base64,${Buffer.from(await result.outputs[0].text()).toString("base64")}`);
const base={task_id:"st_00000001",parent_session_id:"parent",root_session_id:"parent",depth:0,execution_mode:"in-process",model:"faux/faux-1",residency_state:"resident",status:"running",created_at:"1970-01-01T00:00:01.000Z",updated_at:"1970-01-01T00:00:01.000Z",notification:{run_epoch:0,notified_epoch:-1},notify_on_terminal:true};
const stats={runtime_ms:1000,turns:2,tool_calls:3,total_tokens:200,output_tokens:50,generation_ms:400,tokens_per_second:125,cost_usd:0.12};
const cases=[
    {name:"plain",record:base,activity:"running"},
    {name:"summary",record:{...base,task_summary:"Inspect task continuation flow",category:"quick"},activity:"read src/lib.rs",stats},
    {name:"suspended",record:{...base,name:"waiting child",residency_state:"persisted_only"},activity:"reading"},
    {name:"wide-control",record:{...base,task_summary:"한글 넓은 이름\u001b[31m end",description:"ignored description",agent_type:"explore"},activity:"read\nnext"},
];
const outputs=cases.map(test=>({...test,renders:[40,80,120].map(width=>({width,rows:status.backgroundWidgetRows([test.record],new Map([[base.task_id,test.activity]]),2250,()=>test.stats,width)})),staticRows:status.buildWidgetRows([test.record]),taskRow:status.formatTaskRow(test.record)}));
const out=resolve(import.meta.dir,"../../crates/omo/components/maho-omo-task/tests/golden"); mkdirSync(out,{recursive:true}); writeFileSync(join(out,"status-rows.json"),JSON.stringify({source:expected,cases:outputs},null,2)+"\n"); console.log(`Generated ${outputs.length*3} background status goldens from ${expected}`);
