<?php
function GADGET($a){ echo "   >>> GADGET EXECUTED (autoloaded) arg='$a' <<<\n"; }
spl_autoload_register(function($c){
    if ($c==='Guarded')   { echo "   [autoloader] loading Guarded\n";   require __DIR__.'/Guarded.php'; }
    if ($c==='Unguarded') { echo "   [autoloader] loading Unguarded\n"; require __DIR__.'/Unguarded.php'; }
});
echo "PHP ".PHP_VERSION."  (classes NOT pre-loaded; only the autoloader can bring them in)\n";
echo "class_exists before? Guarded=".var_export(class_exists('Guarded',false),true)."\n";

echo "\n[1] unserialize Guarded (autoloader loads it) — does __wakeup still fire+block?\n";
$p1='O:7:"Guarded":2:{s:13:"bookmark_name";s:5:"PWNED";s:10:"on_destroy";s:6:"GADGET";}';
try{ unserialize($p1);}catch(\Throwable $e){ echo "   caught: ".$e->getMessage()."\n"; } gc_collect_cycles();

echo "\n[2] CONTROL: unserialize Unguarded (autoloaded, no __wakeup) — gadget SHOULD fire:\n";
$p2='O:9:"Unguarded":2:{s:13:"bookmark_name";s:5:"PWNED2";s:10:"on_destroy";s:6:"GADGET";}';
try{ unserialize($p2);}catch(\Throwable $e){ echo "   caught: ".$e->getMessage()."\n"; } gc_collect_cycles();
echo "\n=== END ===\n";
