<?php
class Guarded {   // == WP_HTML_Token: dangerous __destruct + throwing __wakeup
    public $bookmark_name; public $on_destroy;
    function __construct($b=null,$od=null){ $this->bookmark_name=$b; $this->on_destroy=$od; }
    function __destruct(){ if(is_callable($this->on_destroy)){ echo "   [Guarded::__destruct] firing\n"; call_user_func($this->on_destroy,$this->bookmark_name); } }
    function __wakeup(){ throw new \LogicException('should never be unserialized'); }
}
class NoGuard {   // control: SAME dangerous __destruct, NO __wakeup
    public $bookmark_name; public $on_destroy;
    function __construct($b=null,$od=null){ $this->bookmark_name=$b; $this->on_destroy=$od; }
    function __destruct(){ if(is_callable($this->on_destroy)){ echo "   [NoGuard::__destruct] firing\n"; call_user_func($this->on_destroy,$this->bookmark_name); } }
}
class Container { public $data; }
function GADGET($a){ echo "   >>> GADGET EXECUTED arg='$a' <<<\n"; }
echo "PHP ".PHP_VERSION."\n";
function payload($cls,$name){ $o=new $cls($name,'GADGET'); $s=serialize($o); $o->on_destroy=null; unset($o); return $s; }

echo "\n[A] CONTROL: NoGuard direct unserialize (no __wakeup) -> gadget SHOULD fire:\n";
try{ unserialize(payload('NoGuard','A')); }catch(\Throwable $e){ echo "   caught: ".$e->getMessage()."\n"; } gc_collect_cycles();

echo "\n[B] Guarded direct unserialize (throwing __wakeup):\n";
try{ unserialize(payload('Guarded','B')); }catch(\Throwable $e){ echo "   caught wakeup: ".$e->getMessage()."\n"; } gc_collect_cycles();

echo "\n[C] Guarded NESTED in a Container (does nesting bypass its __wakeup?):\n";
$c=new Container(); $c->data=new Guarded('C-nested','GADGET'); $cs=serialize($c); $c->data->on_destroy=null; unset($c);
try{ unserialize($cs); }catch(\Throwable $e){ echo "   caught wakeup: ".$e->getMessage()."\n"; } gc_collect_cycles();

echo "\n[D] NoGuard NESTED in a Container -> gadget SHOULD fire:\n";
$c2=new Container(); $c2->data=new NoGuard('D-nested','GADGET'); $cs2=serialize($c2); $c2->data->on_destroy=null; unset($c2);
try{ unserialize($cs2); }catch(\Throwable $e){ echo "   caught: ".$e->getMessage()."\n"; } gc_collect_cycles();
echo "\n=== END ".PHP_VERSION." ===\n";
