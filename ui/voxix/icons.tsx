import {
  ArrowDownCircle as LucideArrowDownCircle,
  AudioWaveform,
  Bird,
  Box,
  Brain,
  Check,
  CheckCircle2,
  Circle,
  CircleEllipsis,
  Clock,
  ClipboardCopy,
  Copy,
  Cpu,
  Globe,
  Hand,
  Info,
  Layers,
  Lock,
  Mic,
  Moon,
  Music,
  Pause,
  Play,
  RefreshCw,
  Server,
  Settings,
  Sparkles,
  SpellCheck,
  Trash2,
  TriangleAlert,
  Volume2,
  VolumeX,
  XCircle,
  Menu,
  X,
  type LucideIcon,
} from "lucide-react";

const sized =
  (Icon: LucideIcon) =>
  ({ size = 16, className }: { size?: number; className?: string }) => (
    <Icon size={size} className={className} strokeWidth={1.8} />
  );

export const ArrowDownCircle = sized(LucideArrowDownCircle);
export const CheckmarkCircle = sized(CheckCircle2);
export const CheckIcon = sized(Check);
export { Circle };
export const EllipsisCircle = sized(CircleEllipsis);
export const ExclamationTriangle = sized(TriangleAlert);
export const LockShield = sized(Lock);
export const Waveform = sized(AudioWaveform);
export const XmarkCircle = sized(XCircle);
export const GearIcon = sized(Settings);
export const ClockIcon = sized(Clock);
export const InfoIcon = sized(Info);
export const TransformsIcon = sized(SpellCheck);
export const MicIcon = sized(Mic);
export const GlobeIcon = sized(Globe);
export const HandTapIcon = sized(Hand);
export const VolumeIcon = sized(Volume2);
export const MuteIcon = sized(VolumeX);
export const CopyIcon = sized(Copy);
export const ClipboardIcon = sized(ClipboardCopy);
export const PlayIcon = sized(Play);
export const PauseIcon = sized(Pause);
export const TrashIcon = sized(Trash2);
export const RefreshIcon = sized(RefreshCw);
export const ServerIcon = sized(Server);
export const ParakeetIcon = sized(Bird);
export const CanaryIcon = sized(Bird);
export const WhisperIcon = sized(Mic);
export const VoxtralIcon = sized(Sparkles);
export const QwenIcon = sized(Brain);
export const GraniteIcon = sized(Box);
export const GigaIcon = sized(Music);
export const MoonshineIcon = sized(Moon);
export const CohereIcon = sized(Layers);
export const FunAsrIcon = sized(AudioWaveform);
export const NemotronIcon = sized(Cpu);
export const GenericModelIcon = sized(Box);
export const MenuIcon = sized(Menu);
export const XIcon = sized(X);
