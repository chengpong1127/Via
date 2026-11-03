import AVFoundation
import UIKit

final class SpokenGuidance: NSObject, NavigationFeedback {
    private let synthesizer = AVSpeechSynthesizer()
    private let session = AVAudioSession.sharedInstance()
    private let haptics = UINotificationFeedbackGenerator()
    private let impact = UIImpactFeedbackGenerator(style: .heavy)

    override init() {
        super.init()
        synthesizer.delegate = self
    }

    func start(destination: String, status: NavigationStatus) {
        try? session.setCategory(.playback, mode: .voicePrompt, options: [.duckOthers, .interruptSpokenAudioAndMixWithOthers])
        haptics.prepare()
    }

    func update(_ status: NavigationStatus) {}

    func cue(_ cue: NavigationCue) {
        vibrate(for: cue)
        speak(cue.message)
    }

    func stop(_ status: NavigationStatus) {
        synthesizer.stopSpeaking(at: .word)
        try? session.setActive(false, options: .notifyOthersOnDeactivation)
    }

    private func speak(_ text: String) {
        try? session.setActive(true)
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = AVSpeechSynthesisVoice(language: Locale.preferredLanguages.first)
        utterance.prefersAssistiveTechnologySettings = true
        synthesizer.speak(utterance)
    }

    private func vibrate(for cue: NavigationCue) {
        switch cue {
        case .alert(.getOff), .alert(.nextStop):
            Task {
                for _ in 0..<3 {
                    impact.impactOccurred(intensity: 1)
                    try? await Task.sleep(for: .milliseconds(180))
                }
            }
        case .alert(.hurry), .rerouteFailed:
            haptics.notificationOccurred(.warning)
        case .alert(.arrived):
            haptics.notificationOccurred(.success)
        case .maneuver, .alert, .rerouted:
            impact.impactOccurred(intensity: 0.6)
        }
    }
}

extension SpokenGuidance: AVSpeechSynthesizerDelegate {
    nonisolated func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didFinish utterance: AVSpeechUtterance) {
        Task { @MainActor in
            guard !self.synthesizer.isSpeaking else { return }
            try? self.session.setActive(false, options: .notifyOthersOnDeactivation)
        }
    }
}
