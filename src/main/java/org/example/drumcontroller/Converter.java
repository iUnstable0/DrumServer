package org.example.drumcontroller;

import javax.sound.midi.*;
import java.io.File;
import java.io.IOException;
import java.nio.file.Path;
import java.util.*;
import org.springframework.web.bind.annotation.RequestMapping;
import org.example.drumcontroller.FileUploadController;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class Converter {
    private final StorageService storageService;

    public Converter(StorageService storageService) {
        this.storageService = storageService;
    }

    FileUploadController fileUploadController;
    private static final Map<Integer, String> GM_DRUM_MAP = Map.ofEntries(
            Map.entry(35, "Kick"),
            Map.entry(36, "Kick"),
            Map.entry(37, "Snare"),
            Map.entry(38, "Snare"),
            Map.entry(39, "Snare"),
            Map.entry(40, "Snare"),
            Map.entry(41, "Floor Tom"),
            Map.entry(42, "Closed Hi-Hat"),
            Map.entry(43, "Floor Tom"),
            Map.entry(44, "Closed Hi-Hat"),
            Map.entry(45, "Tom 1"),
            Map.entry(46, "Open Hi-Hat"),
            Map.entry(47, "Tom 2"),
            Map.entry(48, "Tom 2"),
            Map.entry(49, "Crash"),
            Map.entry(50, "Tom 2"),
            Map.entry(51, "Ride"),
            Map.entry(52, "Crash"),
            Map.entry(53, "Ride"),
            Map.entry(54, "Crash"),
            Map.entry(55, "Crash"),
            Map.entry(56, "Snare"),
            Map.entry(57, "Crash"),
            Map.entry(58, "Snare"),
            Map.entry(59, "Ride"),
            Map.entry(60, "Tom 1"),
            Map.entry(61, "Tom 1"),
            Map.entry(62, "Snare"),
            Map.entry(63, "Snare"),
            Map.entry(64, "Floor Tom"),
            Map.entry(65, "Tom 2"),
            Map.entry(66, "Floor Tom"),
            Map.entry(67, "Crash"),
            Map.entry(68, "Crash"),
            Map.entry(69, "Closed Hi-Hat"),
            Map.entry(70, "Closed Hi-Hat"),
            Map.entry(71, "Crash"),
            Map.entry(72, "Crash"),
            Map.entry(73, "Closed Hi-Hat"),
            Map.entry(74, "Open Hi-Hat"),
            Map.entry(75, "Snare"),
            Map.entry(76, "Tom 1"),
            Map.entry(77, "Floor Tom"),
            Map.entry(78, "Snare"),
            Map.entry(79, "Snare"),
            Map.entry(80, "Crash"),
            Map.entry(81, "Crash")
    );
    public void convert(String filepath) throws MidiUnavailableException, InvalidMidiDataException, IOException {

        Sequencer sequencer = MidiSystem.getSequencer(false);
        sequencer.open();

        Sequence sequence = MidiSystem.getSequence(
                new File(filepath)
        );
        sequencer.setSequence(sequence);

        Set<Integer> drumChannels = detectDrumChannels(sequence);

        System.out.println("channels" + drumChannels);

        Receiver receiver = new Receiver() {
            @Override
            public void send(MidiMessage message, long timeStamp) {

                if (!(message instanceof ShortMessage sm)) return;

                if (sm.getCommand() == ShortMessage.NOTE_ON &&
                        sm.getData2() > 0 &&
                        drumChannels.contains(sm.getChannel())) {

                    int note = sm.getData1();
                    int velocity = sm.getData2();

                    String drum = GM_DRUM_MAP.getOrDefault(
                            note,
                            "Unknown Drum (" + note + ")"
                    );

                    System.out.println(
                           drum
                    );
                    if (drum.equals("Floor Tom")){
                        //insert here piece to make LED or Pneumatic
                    }
                    if (drum.equals("Snare")){
                        //insert here piece to make LED or Pneumatic
                    }
                    if (drum.equals("Tom 1")){
                        //insert here piece to make LED or Pneumatic
                    }
                    if (drum.equals("Tom 2")){
                        //insert here piece to make LED or Pneumatic
                    }
                    if (drum.equals("Open Hi-Hat")){
                        //insert here piece to make LED or Pneumatic
                    }
                    if (drum.equals("Closed Hi-Hat")){
                        //insert here piece to make LED or Pneumatic
                    }
                    if (drum.equals("Kick")){
                        //insert here piece to make LED or Pneumatic
                    }
                    if (drum.equals("Crash")){
                        //insert here piece to make LED or Pneumatic
                    }
                    if (drum.equals("Ride")){
                        //insert here piece to make LED or Pneumatic
                    }
                }
            }

            @Override
            public void close() {
            }
        };

        sequencer.getTransmitter().setReceiver(receiver);
        sequencer.start();
    }

    private static Set<Integer> detectDrumChannels(Sequence sequence) {

        Map<Integer, Integer> channelHitCount = new HashMap<>();

        for (Track track : sequence.getTracks()) {
            for (int i = 0; i < track.size(); i++) {
                MidiEvent event = track.get(i);
                MidiMessage msg = event.getMessage();

                if (msg instanceof ShortMessage sm &&
                        sm.getCommand() == ShortMessage.NOTE_ON &&
                        sm.getData2() > 0) {

                    int note = sm.getData1();
                    int channel = sm.getChannel();

                    if (note >= 35 && note <= 81) {
                        channelHitCount.merge(channel, 1, Integer::sum);
                    }
                }
            }
        }

        Set<Integer> drumChannels = new HashSet<>();
        for (var entry : channelHitCount.entrySet()) {
            if (entry.getValue() >= 5) { // threshold
                drumChannels.add(entry.getKey());
            }
        }

        return drumChannels;
    }

    @RequestMapping("/play")
    public String play()
            throws MidiUnavailableException, InvalidMidiDataException, IOException {

        Path path = storageService.getLastUploadedPath();

        if (path == null) {
            return "No file uploaded.";
        }

        convert(path.toString());

        return "played";
    }

}